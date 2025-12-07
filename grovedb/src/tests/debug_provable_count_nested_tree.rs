use crate::{tests::make_empty_grovedb, Element, GroveDb};

#[test]
fn debug_provable_count_tree_nested_tree_issue() {
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

    let item2_key = b"carol";
    let item2_value = b"value2";
    db.insert::<&[u8], _>(
        [parent_tree_key.as_slice(), nested_tree_key.as_slice()].as_ref(),
        item2_key,
        Element::new_item(item2_value.to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert item");

    // Get the root hash before proof generation
    let root_hash_before = db.root_hash(None, grove_version).unwrap().unwrap();
    println!("Root hash before proof: {}", hex::encode(&root_hash_before));

    // Create a path query for items in the nested tree
    use grovedb_merk::proofs::{query::query_item::QueryItem, Query};

    use crate::PathQuery;

    let path = vec![parent_tree_key.to_vec(), nested_tree_key.to_vec()];
    let path_query = PathQuery::new(
        path.clone(),
        crate::SizedQuery::new(
            Query::new_single_query_item(QueryItem::Key(item1_key.to_vec())),
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
    use grovedb_merk::proofs::query::VerifyOptions;
    let result = GroveDb::verify_query_with_options(
        &proof,
        &path_query,
        VerifyOptions {
            absence_proofs_for_non_existing_searched_keys: false,
            verify_proof_succinctness: false,
            include_empty_trees_in_result: false,
        },
        grove_version,
    );

    match result {
        Ok((verified_hash, result)) => {
            println!("Verified hash: {}", hex::encode(&verified_hash));
            assert_eq!(root_hash_before, verified_hash, "Root hash mismatch!");
            assert_eq!(result.len(), 1);
        }
        Err(e) => {
            panic!("Proof verification failed: {:?}", e);
        }
    }
}
