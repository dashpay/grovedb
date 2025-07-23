//! Debug test to investigate ProvableCountTree hash issues

#[cfg(test)]
mod tests {
    use grovedb_merk::tree::value_hash;
    use grovedb_version::version::GroveVersion;

    use crate::{tests::make_test_grovedb, Element};

    #[test]
    fn debug_provable_count_tree_hash_calculation() {
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

        // Get the count tree element
        let count_tree_elem = db
            .get::<_, &[&[u8]]>(&[], b"count_tree", None, grove_version)
            .unwrap()
            .expect("should get count tree element");

        println!("Count tree element: {:?}", count_tree_elem);

        // Serialize the element to get its value bytes
        let value_bytes = count_tree_elem
            .serialize(grove_version)
            .expect("should serialize");
        println!("Serialized value bytes: {}", hex::encode(&value_bytes));

        // Calculate the value hash
        let val_hash = value_hash(&value_bytes);
        println!("Value hash: {}", hex::encode(val_hash.value()));

        // Check the feature type for ProvableCountTree
        use grovedb_merk::TreeFeatureType;

        if let Element::ProvableCountTree(_, count, _) = &count_tree_elem {
            println!("\nProvableCountTree count: {}", count);

            // The tree should have ProvableCountedMerkNode feature type
            let feature_type = TreeFeatureType::ProvableCountedMerkNode(*count);
            println!("Feature type: {:?}", feature_type);
        }
    }
}
