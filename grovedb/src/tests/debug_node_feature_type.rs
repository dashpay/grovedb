//! Debug test to verify nodes get correct feature type in ProvableCountTree

#[cfg(test)]
mod tests {
    use grovedb_merk::{element::tree_type::ElementTreeTypeExtensions, TreeFeatureType};
    use grovedb_version::version::GroveVersion;

    use crate::{tests::make_test_grovedb, Element};

    #[test]
    fn debug_node_feature_type() {
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

        // Query the merk to trigger node loading
        let root_hash = merk.root_hash().unwrap();

        eprintln!("Root hash: {:?}", hex::encode(root_hash));

        // Try to get an item's feature type directly
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
            eprintln!("Feature type for key1 in merk: {:?}", feature_type);
            match feature_type {
                TreeFeatureType::ProvableCountedMerkNode(_) => {
                    eprintln!("✓ Node has correct ProvableCountedMerkNode feature type");
                }
                _ => {
                    eprintln!("✗ Node has wrong feature type: {:?}", feature_type);
                }
            }
        } else {
            eprintln!("Could not get feature type for key1");
        }

        // Test the get_feature_type method directly
        let item = Element::new_item(b"test".to_vec());
        let feature_type = item
            .get_feature_type(grovedb_merk::tree_type::TreeType::ProvableCountTree)
            .unwrap();
        eprintln!(
            "Feature type for item in ProvableCountTree: {:?}",
            feature_type
        );

        // Check if it matches what we expect
        match feature_type {
            TreeFeatureType::ProvableCountedMerkNode(_) => {
                eprintln!("✓ Feature type is correctly ProvableCountedMerkNode");
            }
            _ => {
                panic!("✗ Feature type is wrong: {:?}", feature_type);
            }
        }
    }
}
