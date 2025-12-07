//! Debug test to directly inspect node feature types

#[cfg(test)]
mod tests {
    use grovedb_costs::CostsExt;
    use grovedb_merk::TreeFeatureType;
    use grovedb_version::version::GroveVersion;

    use crate::{tests::make_test_grovedb, Element};

    #[test]
    fn debug_node_feature_type_direct() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert a ProvableCountTree
        db.insert(
            &[] as &[&[u8]],
            b"pcount",
            Element::empty_provable_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert provable count tree");

        // Insert items
        for i in 0..3 {
            db.insert(
                &[b"pcount"],
                &format!("key{}", i).into_bytes(),
                Element::new_item(format!("value{}", i).into_bytes()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert item");
        }

        // Open the merk and examine nodes
        let tx = db.start_transaction();
        let merk = db
            .open_transactional_merk_at_path(
                vec![b"pcount".as_slice()].as_slice().into(),
                &tx,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should open merk");

        eprintln!("Merk tree type: {:?}", merk.tree_type);

        // Check if we can get the feature type
        let feature_type_result = merk
            .get_feature_type(
                b"key1",
                true,
                None::<
                    fn(
                        &[u8],
                        &GroveVersion,
                    ) -> Option<grovedb_merk::tree::kv::ValueDefinedCostType>,
                >,
                grove_version,
            )
            .unwrap();
        if let Ok(Some(feature_type)) = feature_type_result {
            eprintln!("Node key1 feature type: {:?}", feature_type);
            match feature_type {
                TreeFeatureType::ProvableCountedMerkNode(count) => {
                    eprintln!(
                        "✓ Node key1 has correct ProvableCountedMerkNode feature type with count: \
                         {}",
                        count
                    );
                }
                _ => {
                    eprintln!("✗ Node key1 has wrong feature type: {:?}", feature_type);
                }
            }
        } else {
            eprintln!("Could not get feature type for key1");
        }

        // Test the get_feature_type method directly on an element
        let item = Element::new_item(b"test".to_vec());
        let feature_type = item
            .get_feature_type(grovedb_merk::tree_type::TreeType::ProvableCountTree)
            .unwrap();
        eprintln!(
            "Feature type for item element in ProvableCountTree context: {:?}",
            feature_type
        );

        // Check if it matches what we expect
        match feature_type {
            TreeFeatureType::ProvableCountedMerkNode(_) => {
                eprintln!("✓ Element's get_feature_type correctly returns ProvableCountedMerkNode");
            }
            _ => {
                panic!(
                    "✗ Element's get_feature_type returns wrong type: {:?}",
                    feature_type
                );
            }
        }
    }
}
