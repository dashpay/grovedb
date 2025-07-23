//! Debug test for ProvableCountTree proof verification with subtrees

#[cfg(test)]
mod tests {
    use grovedb_version::version::GroveVersion;

    use crate::{tests::make_test_grovedb, Element, GroveDb, PathQuery, Query, SizedQuery};

    #[test]
    fn debug_provable_count_tree_with_subtree_verification() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Create a ProvableCountTree at root
        db.insert::<_, &[&[u8]]>(
            &[],
            b"count_tree",
            Element::empty_provable_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert count tree");

        // Add a subtree under the count tree
        db.insert::<_, &[&[u8]]>(
            &[b"count_tree"],
            b"subtree",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree");

        // Add items to the subtree
        db.insert::<_, &[&[u8]]>(
            &[b"count_tree", b"subtree"],
            b"item1",
            Element::new_item(vec![1, 2, 3]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        // Create a path query that queries for the count_tree itself
        let path_query = PathQuery::new(
            vec![],
            SizedQuery::new(
                Query::new_single_key(b"count_tree".to_vec()),
                Some(10),
                None,
            ),
        );

        println!("\n=== Tree structure ===");
        println!("Root -> count_tree (ProvableCountTree)");
        println!("         -> subtree (Tree)");
        println!("             -> item1 (Item)");

        println!("\n=== Generating proof ===");
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should generate proof");

        println!("Proof size: {} bytes", proof.len());

        // Try to verify the proof
        println!("\n=== Verifying proof ===");
        match GroveDb::verify_query(&proof, &path_query, grove_version) {
            Ok((root_hash, results)) => {
                println!("✅ Verification successful!");
                println!("Root hash: {}", hex::encode(root_hash));
                println!("Results count: {}", results.len());
                for (i, result) in results.iter().enumerate() {
                    println!(
                        "Result {}: path={:?}, key={}",
                        i,
                        result.0.iter().map(hex::encode).collect::<Vec<_>>(),
                        hex::encode(&result.1)
                    );
                }
            }
            Err(e) => {
                println!("❌ Verification failed: {:?}", e);

                // Enable proof debugging
                std::env::set_var("GROVEDB_PROOF_DEBUG", "1");

                println!("\n=== Retrying with debug output ===");
                let _ = GroveDb::verify_query(&proof, &path_query, grove_version);
            }
        }

        // Also test querying inside the count tree
        println!("\n\n=== Testing query inside count tree ===");
        let inner_query = PathQuery::new(
            vec![b"count_tree".to_vec()],
            SizedQuery::new(Query::new_single_key(b"subtree".to_vec()), Some(10), None),
        );

        let inner_proof = db
            .prove_query(&inner_query, None, grove_version)
            .unwrap()
            .expect("should generate proof");

        match GroveDb::verify_query(&inner_proof, &inner_query, grove_version) {
            Ok((root_hash, results)) => {
                println!("✅ Inner verification successful!");
                println!("Root hash: {}", hex::encode(root_hash));
                println!("Results count: {}", results.len());
            }
            Err(e) => {
                println!("❌ Inner verification failed: {:?}", e);

                // Let's verify with proof_debug feature
                use crate::operations::proof::{GroveDBProof, GroveDBProofV0};

                let config = bincode::config::standard()
                    .with_big_endian()
                    .with_no_limit();
                let decoded_proof: GroveDBProof = bincode::decode_from_slice(&inner_proof, config)
                    .expect("should decode proof")
                    .0;

                if let GroveDBProof::V0(proof_v0) = &decoded_proof {
                    println!("\n=== Proof structure ===");
                    println!(
                        "Root layer proof size: {} bytes",
                        proof_v0.root_layer.merk_proof.len()
                    );
                    println!(
                        "Number of lower layers: {}",
                        proof_v0.root_layer.lower_layers.len()
                    );
                    for (key, layer) in &proof_v0.root_layer.lower_layers {
                        println!(
                            "  Lower layer for key '{}': proof size = {} bytes",
                            hex::encode(key),
                            layer.merk_proof.len()
                        );
                    }
                }

                // Get the actual subtree hash
                let subtree_elem = db
                    .get(&[b"count_tree"], b"subtree", None, grove_version)
                    .unwrap()
                    .expect("should get subtree element");
                println!("\nSubtree element: {:?}", subtree_elem);

                // Get the count tree element
                let count_tree_elem = db
                    .get::<_, &[&[u8]]>(&[], b"count_tree", None, grove_version)
                    .unwrap()
                    .expect("should get count tree element");
                println!("Count tree element: {:?}", count_tree_elem);
            }
        }
    }
}
