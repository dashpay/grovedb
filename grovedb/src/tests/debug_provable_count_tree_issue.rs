use grovedb_merk::proofs::Query;
use grovedb_version::version::GroveVersion;

use crate::{tests::make_deep_tree, Element, GroveDb, PathQuery};

#[test]
fn debug_provable_count_tree_issue() {
    let grove_version = GroveVersion::latest();
    let db = make_deep_tree(grove_version);

    // Create a ProvableCountTree with initial root key
    let element = Element::new_provable_count_tree(Some(b"initial".to_vec()));
    db.insert(
        &[] as &[&[u8]],
        b"verified",
        element,
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert tree");

    // Check the element in parent tree before adding items
    let tree_element_before = db
        .get(&[] as &[&[u8]], b"verified", None, grove_version)
        .unwrap()
        .expect("should get element");

    println!("Tree element before inserts: {:?}", tree_element_before);

    // Get root hash before
    let root_hash_before = db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("should get root hash");
    println!("Root hash before: {}", hex::encode(&root_hash_before));

    // Insert some items
    for i in 1..=4 {
        let key = format!("key{}", i);
        let value = format!("value{}", i);
        db.insert(
            &[b"verified"],
            key.as_bytes(),
            Element::new_item(value.as_bytes().to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");
    }

    // Check the element in parent tree after adding items
    let tree_element_after = db
        .get(&[] as &[&[u8]], b"verified", None, grove_version)
        .unwrap()
        .expect("should get element");

    println!("\nTree element after inserts: {:?}", tree_element_after);

    // Get root hash after
    let root_hash_after = db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("should get root hash");
    println!("Root hash after: {}", hex::encode(&root_hash_after));

    // The tree element should have been updated with new count
    if let (
        Element::ProvableCountTree(_, count_before, _),
        Element::ProvableCountTree(_, count_after, _),
    ) = (&tree_element_before, &tree_element_after)
    {
        println!(
            "\nCount before: {}, Count after: {}",
            count_before, count_after
        );
        assert_ne!(count_before, count_after, "Count should have been updated");
        assert_eq!(*count_after, 4, "Count should be 4 after inserting 4 items");
    } else {
        panic!("Expected ProvableCountTree elements");
    }

    // Generate and verify a proof
    let path_query = PathQuery::new_unsized(
        vec![b"verified".to_vec()],
        Query::new_single_key(b"key1".to_vec()),
    );

    let proof = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .expect("should generate proof");

    println!("\nProof length: {} bytes", proof.len());

    // Verify the proof
    let (verified_root_hash, proved_values) =
        GroveDb::verify_query_raw(&proof, &path_query, grove_version).expect("should verify proof");

    println!("Verified root hash: {}", hex::encode(&verified_root_hash));
    println!("Current root hash: {}", hex::encode(&root_hash_after));

    assert_eq!(
        verified_root_hash, root_hash_after,
        "Root hashes should match"
    );
}
