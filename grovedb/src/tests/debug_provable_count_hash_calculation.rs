use crate::{tests::make_empty_grovedb, Element, GroveDb};

#[test]
fn debug_provable_count_tree_hash_calculation() {
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

    println!("Step 1: Created empty provable count tree");
    let root_hash = db.root_hash(None, grove_version).unwrap().unwrap();
    println!(
        "Root hash after creating empty tree: {}",
        hex::encode(&root_hash)
    );

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

    println!("\nStep 2: Created nested tree 'accounts'");
    let root_hash = db.root_hash(None, grove_version).unwrap().unwrap();
    println!(
        "Root hash after creating nested tree: {}",
        hex::encode(&root_hash)
    );

    // Get the element directly using the public API
    let elem = db
        .get::<&[u8], _>(&[], parent_tree_key, None, grove_version)
        .unwrap()
        .expect("should get element");
    println!(
        "\nProvableCountTree element after adding nested tree: {:?}",
        elem
    );

    // Now add an item to see how the hash changes
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

    println!("\nStep 3: Added item to nested tree");
    let root_hash = db.root_hash(None, grove_version).unwrap().unwrap();
    println!("Root hash after adding item: {}", hex::encode(&root_hash));

    // Check the provable count tree again
    let elem = db
        .get::<&[u8], _>([].as_ref(), parent_tree_key, None, grove_version)
        .unwrap()
        .expect("should get element");
    println!("\nProvableCountTree element after adding item: {:?}", elem);
}
