use crate::{tests::make_empty_grovedb, Element, GroveDb};

#[test]
fn debug_provable_count_alice_direct() {
    let grove_version = &grovedb_version::version::GroveVersion::latest();
    let db = make_empty_grovedb();

    // Create a regular tree at root (not provable count)
    let tree_key = b"accounts";
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

    // Insert alice
    let item_key = b"alice";
    let item_value = b"value1";
    db.insert::<&[u8], _>(
        [tree_key.as_slice()].as_ref(),
        item_key,
        Element::new_item(item_value.to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert item");

    // Query for alice directly
    use grovedb_merk::proofs::{query::query_item::QueryItem, Query};

    use crate::PathQuery;

    let path_query = PathQuery::new(
        vec![tree_key.to_vec()],
        crate::SizedQuery::new(
            Query::new_single_query_item(QueryItem::Key(item_key.to_vec())),
            None,
            None,
        ),
    );

    println!(
        "Query path: {:?}, item: {:?}",
        path_query
            .path
            .iter()
            .map(|p| std::str::from_utf8(p).unwrap())
            .collect::<Vec<_>>(),
        std::str::from_utf8(item_key).unwrap()
    );

    // Generate and verify proof
    let proof = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .unwrap();

    match GroveDb::verify_query(&proof, &path_query, grove_version) {
        Ok((_, results)) => {
            println!("Simple query results: {}", results.len());
            assert_eq!(results.len(), 1, "Should find alice");
        }
        Err(e) => {
            panic!("Simple query failed: {:?}", e);
        }
    }
}
