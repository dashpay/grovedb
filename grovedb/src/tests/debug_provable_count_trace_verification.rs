use crate::{tests::make_empty_grovedb, Element, GroveDb};

#[test]
fn debug_provable_count_trace_verification() {
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

    // Insert items into the nested tree
    let item1_key = b"alice";
    let item1_value = b"value1";
    db.insert::<&[u8], _>(
        [parent_tree_key.as_slice(), nested_tree_key.as_slice()].as_ref(),
        item1_key,
        Element::new_item(item1_value.to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert item");

    // Create a path query for items in the nested tree
    use grovedb_merk::proofs::{query::query_item::QueryItem, Query};

    use crate::PathQuery;

    // Query for alice at ["verified", "accounts"]
    let path_query = PathQuery::new(
        vec![parent_tree_key.to_vec(), nested_tree_key.to_vec()],
        crate::SizedQuery::new(
            Query::new_single_query_item(QueryItem::Key(item1_key.to_vec())),
            None,
            None,
        ),
    );

    println!(
        "PathQuery: path={:?}, query_item=Key({:?})",
        path_query
            .path
            .iter()
            .map(|p| String::from_utf8_lossy(p))
            .collect::<Vec<_>>(),
        std::str::from_utf8(item1_key).unwrap()
    );

    // Let's check what query_items_at_path returns for each level
    {
        let path = vec![];
        let query = path_query
            .query_items_at_path(&path, grove_version)
            .unwrap();
        println!("\nAt path []: {:?}", query);
    }
    {
        let path = vec![parent_tree_key.as_slice()];
        let query = path_query
            .query_items_at_path(&path, grove_version)
            .unwrap();
        println!("At path [\"verified\"]: {:?}", query);
    }
    {
        let path = vec![parent_tree_key.as_slice(), nested_tree_key.as_slice()];
        let query = path_query
            .query_items_at_path(&path, grove_version)
            .unwrap();
        println!("At path [\"verified\", \"accounts\"]: {:?}", query);
    }

    // Generate proof
    let proof = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .unwrap();

    // Decode and print the proof structure
    let config = bincode::config::standard()
        .with_big_endian()
        .with_no_limit();
    let decoded_proof: crate::operations::proof::GroveDBProof =
        bincode::decode_from_slice(&proof, config).unwrap().0;

    if let crate::operations::proof::GroveDBProof::V0(proof_v0) = decoded_proof {
        println!("\nProof structure:");
        println!(
            "Root layer merk proof ops: {:?}",
            proof_v0.root_layer.merk_proof.len()
        );
        for (i, op) in proof_v0.root_layer.merk_proof.iter().enumerate() {
            println!("  {}: {:?}", i, op);
        }

        for (key, lower_layer) in &proof_v0.root_layer.lower_layers {
            println!(
                "\nLower layer for key {:?}:",
                std::str::from_utf8(key).unwrap()
            );
            println!("  Merk proof ops: {:?}", lower_layer.merk_proof.len());
            for (i, op) in lower_layer.merk_proof.iter().enumerate() {
                println!("    {}: {:?}", i, op);
            }

            for (key2, lower_layer2) in &lower_layer.lower_layers {
                println!(
                    "\n  Lower layer for key {:?}:",
                    std::str::from_utf8(key2).unwrap()
                );
                println!("    Merk proof ops: {:?}", lower_layer2.merk_proof.len());
                for (i, op) in lower_layer2.merk_proof.iter().enumerate() {
                    println!("      {}: {:?}", i, op);
                }
            }
        }
    }

    // Try to verify the proof
    match GroveDb::verify_query(&proof, &path_query, grove_version) {
        Ok((verified_hash, results)) => {
            println!("\nVerification successful!");
            println!("Results count: {}", results.len());
            for (i, (path, key, element)) in results.iter().enumerate() {
                println!(
                    "Result {}: path={:?}, key={:?}, element={:?}",
                    i,
                    path.iter()
                        .map(|p| String::from_utf8_lossy(p))
                        .collect::<Vec<_>>(),
                    std::str::from_utf8(key).unwrap_or(&hex::encode(key)),
                    element
                );
            }
        }
        Err(e) => {
            println!("\nVerification failed: {:?}", e);
        }
    }
}
