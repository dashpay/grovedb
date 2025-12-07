use grovedb_path::SubtreePath;

use crate::{tests::make_empty_grovedb, Element, Error, GroveDb};

#[test]
fn debug_provable_count_tree_hash_issue() {
    let grove_version = &grovedb_version::version::GroveVersion::latest();
    let db = make_empty_grovedb();

    // Create a provable count tree at root
    let provable_count_tree_key = b"provable_count_tree";
    db.insert::<&[u8], _>(
        &[],
        provable_count_tree_key,
        Element::empty_provable_count_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert tree");

    // Insert an item into the provable count tree
    let item_key = b"item1";
    let item_value = b"value1";
    db.insert::<&[u8], _>(
        [provable_count_tree_key.as_slice()].as_ref(),
        item_key,
        Element::new_item(item_value.to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert item");

    // Get the root hash before proof generation
    let root_hash_before = db.root_hash(None, grove_version).unwrap().unwrap();
    println!("Root hash before proof: {}", hex::encode(&root_hash_before));

    // Create a simple path query
    let path = vec![provable_count_tree_key.to_vec()];
    use grovedb_merk::proofs::{query::query_item::QueryItem, Query};

    use crate::{query_result_type::QueryResultType, PathQuery};

    let path_query = PathQuery::new(
        path.clone(),
        crate::SizedQuery::new(
            Query::new_single_query_item(QueryItem::Key(item_key.to_vec())),
            Some(10),
            None,
        ),
    );

    // Generate proof
    let proof = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .unwrap();
    println!("Proof generated, size: {} bytes", proof.len());

    // Try to verify the proof with debug features
    use grovedb_merk::proofs::query::VerifyOptions;
    let (verified_hash, result) = GroveDb::verify_query_with_options(
        &proof,
        &path_query,
        VerifyOptions::default(),
        grove_version,
    )
    .expect("should verify proof");

    println!("Verified hash: {}", hex::encode(&verified_hash));
    assert_eq!(root_hash_before, verified_hash, "Root hash mismatch!");

    // Check the result
    assert_eq!(result.len(), 1);
    let (result_path, result_key, result_element) = &result[0];
    assert_eq!(result_path, &path);
    assert_eq!(result_key, item_key);
    assert!(result_element.is_some());
}
