use crate::{tests::make_empty_grovedb, Element, GroveDb};

#[test]
fn debug_provable_count_tree_query_nested_tree() {
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

    // Get the root hash before proof generation
    let root_hash_before = db.root_hash(None, grove_version).unwrap().unwrap();
    println!("Root hash before proof: {}", hex::encode(&root_hash_before));

    // Create a path query for the nested tree itself
    use grovedb_merk::proofs::{query::query_item::QueryItem, Query};

    use crate::PathQuery;

    // Query for the "accounts" tree inside "verified"
    let path_query = PathQuery::new(
        vec![parent_tree_key.to_vec()],
        crate::SizedQuery::new(
            Query::new_single_query_item(QueryItem::Key(nested_tree_key.to_vec())),
            None,
            None,
        ),
    );

    // Generate proof
    let proof = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .unwrap();
    println!("Proof generated, size: {} bytes", proof.len());

    // Try to verify the proof
    let result = GroveDb::verify_query(&proof, &path_query, grove_version);

    match result {
        Ok((verified_hash, results)) => {
            println!("Verified hash: {}", hex::encode(&verified_hash));
            println!("Results count: {}", results.len());
            assert_eq!(root_hash_before, verified_hash, "Root hash mismatch!");
            assert_eq!(results.len(), 1, "Expected 1 result");

            // Check that we got the tree element
            let (path, key, element) = &results[0];
            println!(
                "Result path: {:?}",
                path.iter().map(hex::encode).collect::<Vec<_>>()
            );
            println!("Result key: {}", hex::encode(key));
            match element {
                Some(Element::Tree(..)) => println!("Got tree element as expected"),
                _ => panic!("Expected tree element, got: {:?}", element),
            }

            println!("Test PASSED!");
        }
        Err(e) => {
            panic!("Proof verification failed: {:?}", e);
        }
    }
}
