use grovedb_merk::proofs::query::query_item::QueryItem;

use crate::{tests::make_empty_grovedb, Element, PathQuery, Query};

#[test]
fn debug_simple_provable_count_tree() {
    let grove_version = &grovedb_version::version::GroveVersion::latest();
    let db = make_empty_grovedb();

    // Create a simple provable count tree at root without any nested trees
    let tree_key = b"counttree";
    db.insert::<&[u8], _>(
        &[],
        tree_key,
        Element::empty_provable_count_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("should insert tree");

    // Add a single item
    let item_key = b"item1";
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

    // Create a query for the item
    let path_query = PathQuery::new(
        vec![tree_key.to_vec()],
        crate::SizedQuery::new(
            Query::new_single_query_item(QueryItem::Key(item_key.to_vec())),
            None,
            None,
        ),
    );

    // Generate and verify proof
    let proof = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .unwrap();

    match crate::GroveDb::verify_query(&proof, &path_query, grove_version) {
        Ok((root_hash, results)) => {
            println!("Simple test PASSED!");
            println!("Root hash: {}", hex::encode(&root_hash));
            println!("Results count: {}", results.len());
            assert_eq!(results.len(), 1);
        }
        Err(e) => {
            panic!("Simple test FAILED: {:?}", e);
        }
    }
}
