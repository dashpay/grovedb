//! Sum tree tests

#[cfg(test)]
mod tests {
    use grovedb_merk::{
        element::costs::ElementCostExtensions,
        proofs::Query,
        tree::{kv::ValueDefinedCostType, AggregateData},
        TreeFeatureType::{BasicMerkNode, BigSummedMerkNode, SummedMerkNode},
    };
    use grovedb_storage::StorageBatch;
    use grovedb_version::version::GroveVersion;
    use tempfile::TempDir;

    use crate::{
        batch::QualifiedGroveDbOp,
        element::SumValue,
        reference_path::ReferencePathType,
        tests::{make_test_grovedb, TEST_LEAF},
        Element, Error, GroveDb, PathQuery,
    };

    #[test]
    fn test_sum_tree_behaves_like_regular_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");

        // Can fetch sum tree
        let sum_tree = db
            .get([TEST_LEAF].as_ref(), b"key", None, grove_version)
            .unwrap()
            .expect("should get tree");
        assert!(matches!(sum_tree, Element::SumTree(..)));

        db.insert(
            [TEST_LEAF, b"key"].as_ref(),
            b"innerkey",
            Element::new_item(vec![1]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");
        db.insert(
            [TEST_LEAF, b"key"].as_ref(),
            b"innerkey2",
            Element::new_item(vec![3]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");
        db.insert(
            [TEST_LEAF, b"key"].as_ref(),
            b"innerkey3",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        // Test proper item retrieval
        let item = db
            .get(
                [TEST_LEAF, b"key"].as_ref(),
                b"innerkey",
                None,
                grove_version,
            )
            .unwrap()
            .expect("should get item");
        assert_eq!(item, Element::new_item(vec![1]));

        // Test proof generation
        let mut query = Query::new();
        query.insert_key(b"innerkey2".to_vec());

        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"key".to_vec()], query);
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should generate proof");
        let (root_hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(
            root_hash,
            db.grove_db.root_hash(None, grove_version).unwrap().unwrap()
        );
        assert_eq!(result_set.len(), 1);
        assert_eq!(
            Element::deserialize(&result_set[0].value, grove_version)
                .expect("should deserialize element"),
            Element::new_item(vec![3])
        );

        let (root_hash, parent, result_set) =
            GroveDb::verify_query_get_parent_tree_info(&proof, &path_query, grove_version)
                .expect("should verify proof");
        assert_eq!(
            root_hash,
            db.grove_db.root_hash(None, grove_version).unwrap().unwrap()
        );
        assert_eq!(result_set.len(), 1);
        assert_eq!(
            parent,
            SummedMerkNode(0), // because no sum items
        );
    }

    #[test]
    fn test_sum_item_behaves_like_regular_item() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"sumkey",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");
        db.insert(
            [TEST_LEAF, b"sumkey"].as_ref(),
            b"k1",
            Element::new_item(vec![1]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");
        db.insert(
            [TEST_LEAF, b"sumkey"].as_ref(),
            b"k2",
            Element::new_sum_item(5),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");
        db.insert(
            [TEST_LEAF, b"sumkey"].as_ref(),
            b"k3",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");

        // Test proper item retrieval
        let item = db
            .get([TEST_LEAF, b"sumkey"].as_ref(), b"k2", None, grove_version)
            .unwrap()
            .expect("should get item");
        assert_eq!(item, Element::new_sum_item(5));

        // Test proof generation
        let mut query = Query::new();
        query.insert_key(b"k2".to_vec());

        let path_query =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"sumkey".to_vec()], query);
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should generate proof");
        let (root_hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(
            root_hash,
            db.grove_db.root_hash(None, grove_version).unwrap().unwrap()
        );
        assert_eq!(result_set.len(), 1);
        let element_from_proof = Element::deserialize(&result_set[0].value, grove_version)
            .expect("should deserialize element");
        assert_eq!(element_from_proof, Element::new_sum_item(5));
        assert_eq!(element_from_proof.sum_value_or_default(), 5);

        let (root_hash, parent, result_set) =
            GroveDb::verify_query_get_parent_tree_info(&proof, &path_query, grove_version)
                .expect("should verify proof");
        assert_eq!(
            root_hash,
            db.grove_db.root_hash(None, grove_version).unwrap().unwrap()
        );
        assert_eq!(result_set.len(), 1);
        assert_eq!(
            parent,
            SummedMerkNode(5), // because no sum items
        );
    }

    #[test]
    fn test_cannot_insert_sum_item_in_regular_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"sumkey",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");
        assert!(matches!(
            db.insert(
                [TEST_LEAF, b"sumkey"].as_ref(),
                b"k1",
                Element::new_sum_item(5),
                None,
                None,
                grove_version
            )
            .unwrap(),
            Err(Error::InvalidInput("cannot add sum item to non sum tree"))
        ));
        assert!(matches!(
            db.insert(
                [TEST_LEAF, b"sumkey"].as_ref(),
                b"k2",
                Element::ItemWithSumItem(b"value".to_vec(), 10, None),
                None,
                None,
                grove_version
            )
            .unwrap(),
            Err(Error::InvalidInput("cannot add sum item to non sum tree"))
        ));
    }

    #[test]
    fn test_item_with_sum_item_updates_sum_tree_sum() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"sum_mixed",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum tree");

        db.insert(
            [TEST_LEAF, b"sum_mixed"].as_ref(),
            b"alpha",
            Element::ItemWithSumItem(b"payload".to_vec(), 6, None),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item with sum");

        let assert_sum = |expected_sum: i64| {
            let element = db
                .get([TEST_LEAF].as_ref(), b"sum_mixed", None, grove_version)
                .unwrap()
                .expect("should fetch sum tree");
            match element {
                Element::SumTree(_, sum, _) => assert_eq!(sum, expected_sum),
                _ => panic!("expected sum tree"),
            }
        };

        assert_sum(6);

        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec(), b"sum_mixed".to_vec()],
            b"alpha".to_vec(),
            Element::ItemWithSumItem(b"updated".to_vec(), -9, Some(vec![5])),
        )];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("should replace item with sum");

        assert_sum(-9);

        db.delete(
            [TEST_LEAF, b"sum_mixed"].as_ref(),
            b"alpha",
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should delete item");

        assert_sum(0);
    }

    #[test]
    fn test_homogenous_node_type_in_sum_trees_and_regular_trees() {
        let grove_version = GroveVersion::latest();
        // All elements in a sum tree must have a summed feature type
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");
        // Add sum items
        db.insert(
            [TEST_LEAF, b"key"].as_ref(),
            b"item1",
            Element::new_sum_item(30),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");
        db.insert(
            [TEST_LEAF, b"key"].as_ref(),
            b"item2",
            Element::new_sum_item(10),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");
        // Add regular items
        db.insert(
            [TEST_LEAF, b"key"].as_ref(),
            b"item3",
            Element::new_item(vec![10]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");
        db.insert(
            [TEST_LEAF, b"key"].as_ref(),
            b"item4",
            Element::new_item(vec![15]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        let batch = StorageBatch::new();
        let transaction = db.start_transaction();

        // Open merk and check all elements in it
        let merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"key"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");
        assert!(matches!(
            merk.get_feature_type(
                b"item1",
                true,
                None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                grove_version
            )
            .unwrap()
            .expect("node should exist"),
            Some(SummedMerkNode(30))
        ));
        assert!(matches!(
            merk.get_feature_type(
                b"item2",
                true,
                None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                grove_version
            )
            .unwrap()
            .expect("node should exist"),
            Some(SummedMerkNode(10))
        ));
        assert!(matches!(
            merk.get_feature_type(
                b"item3",
                true,
                None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                grove_version
            )
            .unwrap()
            .expect("node should exist"),
            Some(SummedMerkNode(0))
        ));
        assert!(matches!(
            merk.get_feature_type(
                b"item4",
                true,
                None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                grove_version
            )
            .unwrap()
            .expect("node should exist"),
            Some(SummedMerkNode(0))
        ));
        assert_eq!(
            merk.aggregate_data()
                .expect("expected to get sum")
                .as_sum_i64(),
            40
        );

        // Perform the same test on regular trees
        let db = make_test_grovedb(grove_version);
        let transaction = db.start_transaction();

        db.insert(
            [TEST_LEAF].as_ref(),
            b"key",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");
        db.insert(
            [TEST_LEAF, b"key"].as_ref(),
            b"item1",
            Element::new_item(vec![30]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");
        db.insert(
            [TEST_LEAF, b"key"].as_ref(),
            b"item2",
            Element::new_item(vec![10]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        let merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"key"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");
        assert!(matches!(
            merk.get_feature_type(
                b"item1",
                true,
                Some(&Element::value_defined_cost_for_serialized_value),
                grove_version
            )
            .unwrap()
            .expect("node should exist"),
            Some(BasicMerkNode)
        ));
        assert!(matches!(
            merk.get_feature_type(
                b"item2",
                true,
                Some(&Element::value_defined_cost_for_serialized_value),
                grove_version
            )
            .unwrap()
            .expect("node should exist"),
            Some(BasicMerkNode)
        ));
        assert_eq!(
            merk.aggregate_data().expect("expected to get sum"),
            AggregateData::NoAggregateData
        );
    }

    #[test]
    fn test_sum_tree_feature() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");

        let batch = StorageBatch::new();

        let transaction = db.start_transaction();
        // Sum should be non for non sum tree
        // TODO: change interface to retrieve element directly
        let merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"key"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");
        assert_eq!(
            merk.aggregate_data().expect("expected to get sum"),
            AggregateData::NoAggregateData
        );

        // Add sum tree
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key2",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum tree");
        let sum_tree = db
            .get([TEST_LEAF].as_ref(), b"key2", None, grove_version)
            .unwrap()
            .expect("should retrieve tree");
        assert_eq!(sum_tree.sum_value_or_default(), 0);

        // Add sum items to the sum tree
        db.insert(
            [TEST_LEAF, b"key2"].as_ref(),
            b"item1",
            Element::new_sum_item(30),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");
        // TODO: change interface to retrieve element directly
        let merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"key2"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");
        assert_eq!(
            merk.aggregate_data().expect("expected to get sum"),
            AggregateData::Sum(30)
        );

        // Add more sum items
        db.insert(
            [TEST_LEAF, b"key2"].as_ref(),
            b"item2",
            Element::new_sum_item(-10),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");
        db.insert(
            [TEST_LEAF, b"key2"].as_ref(),
            b"item3",
            Element::new_sum_item(50),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");
        let merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"key2"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");
        assert_eq!(
            merk.aggregate_data().expect("expected to get sum"),
            AggregateData::Sum(70)
        ); // 30 - 10 + 50 = 70

        // Add non sum items, result should remain the same
        db.insert(
            [TEST_LEAF, b"key2"].as_ref(),
            b"item4",
            Element::new_item(vec![29]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");
        let merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"key2"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");
        assert_eq!(
            merk.aggregate_data().expect("expected to get sum"),
            AggregateData::Sum(70)
        );

        // Update existing sum items
        db.insert(
            [TEST_LEAF, b"key2"].as_ref(),
            b"item2",
            Element::new_sum_item(10),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");
        db.insert(
            [TEST_LEAF, b"key2"].as_ref(),
            b"item3",
            Element::new_sum_item(-100),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");
        let merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"key2"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");
        assert_eq!(
            merk.aggregate_data().expect("expected to get sum"),
            AggregateData::Sum(-60)
        ); // 30 + 10 - 100 = -60

        // We can not replace a normal item with a sum item, so let's delete it first
        db.delete(
            [TEST_LEAF, b"key2"].as_ref(),
            b"item4",
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected to delete");
        // Use a large value
        db.insert(
            [TEST_LEAF, b"key2"].as_ref(),
            b"item4",
            Element::new_sum_item(10000000),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");
        let merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"key2"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");
        assert_eq!(
            merk.aggregate_data().expect("expected to get sum"),
            AggregateData::Sum(9999940)
        ); // 30 +
           // 10 -
           // 100 +
           // 10000000
    }

    #[test]
    fn test_sum_tree_overflow() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");

        let batch = StorageBatch::new();

        let transaction = db.start_transaction();
        // Sum should be non for non sum tree
        // TODO: change interface to retrieve element directly
        let merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"key"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");
        assert_eq!(
            merk.aggregate_data().expect("expected to get sum"),
            AggregateData::NoAggregateData
        );

        // Add sum tree
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key2",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum tree");
        let sum_tree = db
            .get([TEST_LEAF].as_ref(), b"key2", None, grove_version)
            .unwrap()
            .expect("should retrieve tree");
        assert_eq!(sum_tree.sum_value_or_default(), 0);

        // Add sum items to the sum tree
        db.insert(
            [TEST_LEAF, b"key2"].as_ref(),
            b"item1",
            Element::new_sum_item(SumValue::MAX),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");
        // TODO: change interface to retrieve element directly
        let merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"key2"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");
        assert_eq!(
            merk.aggregate_data().expect("expected to get sum"),
            AggregateData::Sum(SumValue::MAX)
        );

        // Subtract 10 from Max should work
        db.insert(
            [TEST_LEAF, b"key2"].as_ref(),
            b"item2",
            Element::new_sum_item(-10),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");
        let merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"key2"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");
        assert_eq!(
            merk.aggregate_data().expect("expected to get sum"),
            AggregateData::Sum(SumValue::MAX - 10)
        );

        // Add 20 from Max should overflow
        db.insert(
            [TEST_LEAF, b"key2"].as_ref(),
            b"item3",
            Element::new_sum_item(20),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect_err("should not be able to insert item");
        let merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"key2"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");
        assert_eq!(
            merk.aggregate_data().expect("expected to get sum"),
            AggregateData::Sum(SumValue::MAX - 10)
        );

        // Add non sum items, result should remain the same
        db.insert(
            [TEST_LEAF, b"key2"].as_ref(),
            b"item4",
            Element::new_item(vec![29]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");
        let merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"key2"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");
        assert_eq!(
            merk.aggregate_data().expect("expected to get sum"),
            AggregateData::Sum(SumValue::MAX - 10)
        );

        // Update existing sum item will overflow
        db.insert(
            [TEST_LEAF, b"key2"].as_ref(),
            b"item2",
            Element::new_sum_item(10), // we are replacing -10 with 10
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect_err("should not be able to insert item");
        let merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"key2"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");
        assert_eq!(
            merk.aggregate_data().expect("expected to get sum"),
            AggregateData::Sum(SumValue::MAX - 10)
        );

        // Update existing sum item will overflow
        db.insert(
            [TEST_LEAF, b"key2"].as_ref(),
            b"item2",
            Element::new_sum_item(SumValue::MIN), // we are replacing -10 with SumValue::MIN
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should be able to insert item");
        let merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"key2"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");
        assert_eq!(
            merk.aggregate_data().expect("expected to get sum"),
            AggregateData::Sum(-1)
        );

        db.insert(
            [TEST_LEAF, b"key2"].as_ref(),
            b"item3",
            Element::new_sum_item(-40),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should be able to insert item");

        let merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"key2"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");
        assert_eq!(
            merk.aggregate_data().expect("expected to get sum"),
            AggregateData::Sum(-41)
        );

        // Deleting item1 should make us overflow
        db.delete(
            [TEST_LEAF, b"key2"].as_ref(),
            b"item1",
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect_err("expected not be able to delete");
        let merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"key2"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");
        assert_eq!(
            merk.aggregate_data().expect("expected to get sum"),
            AggregateData::Sum(-41)
        );
    }

    #[test]
    fn test_sum_tree_propagation() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        // Tree
        //   SumTree
        //      SumTree
        //        Item1
        //        SumItem1
        //        SumItem2
        //      SumItem3
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");
        db.insert(
            [TEST_LEAF, b"key"].as_ref(),
            b"tree2",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");
        db.insert(
            [TEST_LEAF, b"key"].as_ref(),
            b"sumitem3",
            Element::new_sum_item(20),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");
        db.insert(
            [TEST_LEAF, b"key", b"tree2"].as_ref(),
            b"item1",
            Element::new_item(vec![2]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");
        db.insert(
            [TEST_LEAF, b"key", b"tree2"].as_ref(),
            b"sumitem1",
            Element::new_sum_item(5),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");
        db.insert(
            [TEST_LEAF, b"key", b"tree2"].as_ref(),
            b"sumitem2",
            Element::new_sum_item(10),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");
        db.insert(
            [TEST_LEAF, b"key", b"tree2"].as_ref(),
            b"item2",
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"key".to_vec(),
                b"tree2".to_vec(),
                b"sumitem1".to_vec(),
            ])),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        let sum_tree = db
            .get([TEST_LEAF].as_ref(), b"key", None, grove_version)
            .unwrap()
            .expect("should fetch tree");
        assert_eq!(sum_tree.sum_value_or_default(), 35);

        let batch = StorageBatch::new();
        let transaction = db.start_transaction();

        // Assert node feature types
        let test_leaf_merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");
        assert!(matches!(
            test_leaf_merk
                .get_feature_type(
                    b"key",
                    true,
                    Some(&Element::value_defined_cost_for_serialized_value),
                    grove_version
                )
                .unwrap()
                .expect("node should exist"),
            Some(BasicMerkNode)
        ));

        let parent_sum_tree = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"key"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");
        assert!(matches!(
            parent_sum_tree
                .get_feature_type(
                    b"tree2",
                    true,
                    Some(&Element::value_defined_cost_for_serialized_value),
                    grove_version
                )
                .unwrap()
                .expect("node should exist"),
            Some(SummedMerkNode(15)) /* 15 because the child sum tree has one sum item of
                                      * value 5 and
                                      * another of value 10 */
        ));

        let child_sum_tree = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"key", b"tree2"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");
        assert!(matches!(
            child_sum_tree
                .get_feature_type(
                    b"item1",
                    true,
                    None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                    grove_version
                )
                .unwrap()
                .expect("node should exist"),
            Some(SummedMerkNode(0))
        ));
        assert!(matches!(
            child_sum_tree
                .get_feature_type(
                    b"sumitem1",
                    true,
                    None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                    grove_version
                )
                .unwrap()
                .expect("node should exist"),
            Some(SummedMerkNode(5))
        ));
        assert!(matches!(
            child_sum_tree
                .get_feature_type(
                    b"sumitem2",
                    true,
                    None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                    grove_version
                )
                .unwrap()
                .expect("node should exist"),
            Some(SummedMerkNode(10))
        ));

        // TODO: should references take the sum of the referenced element??
        assert!(matches!(
            child_sum_tree
                .get_feature_type(
                    b"item2",
                    true,
                    None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                    grove_version
                )
                .unwrap()
                .expect("node should exist"),
            Some(SummedMerkNode(0))
        ));
    }

    #[test]
    fn test_big_sum_tree_propagation() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        // Tree
        //   BigSumTree
        //      SumTree1
        //        SumItem1
        //        SumItem2
        //      SumTree2
        //        SumItem3
        //      SumItem4
        db.insert(
            [TEST_LEAF].as_ref(),
            b"big_sum_tree",
            Element::empty_big_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");
        db.insert(
            [TEST_LEAF, b"big_sum_tree"].as_ref(),
            b"sum_tree_1",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");
        db.insert(
            [TEST_LEAF, b"big_sum_tree"].as_ref(),
            b"sum_tree_2",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");
        db.insert(
            [TEST_LEAF, b"big_sum_tree", b"sum_tree_1"].as_ref(),
            b"item1",
            Element::new_item(vec![2]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");
        db.insert(
            [TEST_LEAF, b"big_sum_tree", b"sum_tree_1"].as_ref(),
            b"sum_item_1",
            Element::new_sum_item(SumValue::MAX - 40),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");
        db.insert(
            [TEST_LEAF, b"big_sum_tree", b"sum_tree_1"].as_ref(),
            b"sum_item_2",
            Element::new_sum_item(30),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");
        db.insert(
            [TEST_LEAF, b"big_sum_tree", b"sum_tree_1"].as_ref(),
            b"ref_1",
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"big_sum_tree".to_vec(),
                b"sum_tree_1".to_vec(),
                b"sum_item_1".to_vec(),
            ])),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        db.insert(
            [TEST_LEAF, b"big_sum_tree", b"sum_tree_2"].as_ref(),
            b"sum_item_3",
            Element::new_sum_item(SumValue::MAX - 50),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        let sum_tree = db
            .get([TEST_LEAF].as_ref(), b"big_sum_tree", None, grove_version)
            .unwrap()
            .expect("should fetch tree");
        assert_eq!(
            sum_tree.big_sum_value_or_default(),
            (SumValue::MAX - 10) as i128 + (SumValue::MAX - 50) as i128
        );

        db.insert(
            [TEST_LEAF, b"big_sum_tree"].as_ref(),
            b"sum_item_4",
            Element::new_sum_item(SumValue::MAX - 70),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        let sum_tree = db
            .get([TEST_LEAF].as_ref(), b"big_sum_tree", None, grove_version)
            .unwrap()
            .expect("should fetch tree");
        assert_eq!(
            sum_tree.big_sum_value_or_default(),
            (SumValue::MAX - 10) as i128
                + (SumValue::MAX - 50) as i128
                + (SumValue::MAX - 70) as i128
        );

        let batch = StorageBatch::new();
        let transaction = db.start_transaction();

        // Assert node feature types
        let test_leaf_merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");
        assert!(matches!(
            test_leaf_merk
                .get_feature_type(
                    b"big_sum_tree",
                    true,
                    Some(&Element::value_defined_cost_for_serialized_value),
                    grove_version
                )
                .unwrap()
                .expect("node should exist"),
            Some(BasicMerkNode)
        ));

        let parent_sum_tree = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"big_sum_tree"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");
        let feature_type = parent_sum_tree
            .get_feature_type(
                b"sum_tree_1",
                true,
                Some(&Element::value_defined_cost_for_serialized_value),
                grove_version,
            )
            .unwrap()
            .expect("node should exist")
            .expect("expected feature type");
        assert_eq!(
            feature_type,
            BigSummedMerkNode((SumValue::MAX - 10) as i128)
        );

        let feature_type = parent_sum_tree
            .get_feature_type(
                b"sum_item_4",
                true,
                Some(&Element::value_defined_cost_for_serialized_value),
                grove_version,
            )
            .unwrap()
            .expect("node should exist")
            .expect("expected feature type");
        assert_eq!(
            feature_type,
            BigSummedMerkNode((SumValue::MAX - 70) as i128)
        );

        let child_sum_tree = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"big_sum_tree", b"sum_tree_1"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");
        assert_eq!(
            child_sum_tree
                .get_feature_type(
                    b"item1",
                    true,
                    None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                    grove_version
                )
                .unwrap()
                .expect("node should exist"),
            Some(SummedMerkNode(0))
        );
        assert_eq!(
            child_sum_tree
                .get_feature_type(
                    b"sum_item_1",
                    true,
                    None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                    grove_version
                )
                .unwrap()
                .expect("node should exist"),
            Some(SummedMerkNode(SumValue::MAX - 40))
        );
        assert_eq!(
            child_sum_tree
                .get_feature_type(
                    b"sum_item_2",
                    true,
                    None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                    grove_version
                )
                .unwrap()
                .expect("node should exist"),
            Some(SummedMerkNode(30))
        );

        assert_eq!(
            child_sum_tree
                .get_feature_type(
                    b"ref_1",
                    true,
                    None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                    grove_version
                )
                .unwrap()
                .expect("node should exist"),
            Some(SummedMerkNode(0))
        );

        let child_sum_tree_2 = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"big_sum_tree", b"sum_tree_2"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");

        assert_eq!(
            child_sum_tree_2
                .get_feature_type(
                    b"sum_item_3",
                    true,
                    None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                    grove_version
                )
                .unwrap()
                .expect("node should exist"),
            Some(SummedMerkNode(SumValue::MAX - 50))
        );
    }

    #[test]
    fn test_sum_tree_with_batches() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"key1".to_vec(),
                Element::empty_sum_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"key1".to_vec()],
                b"a".to_vec(),
                Element::new_item(vec![214]),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"key1".to_vec()],
                b"b".to_vec(),
                Element::new_sum_item(10),
            ),
        ];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("should apply batch");
        let transaction = db.start_transaction();

        let batch = StorageBatch::new();
        let sum_tree = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"key1"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");

        assert_eq!(
            sum_tree
                .get_feature_type(
                    b"a",
                    true,
                    Some(&Element::value_defined_cost_for_serialized_value),
                    grove_version
                )
                .unwrap()
                .expect("node should exist"),
            Some(SummedMerkNode(0))
        );
        assert_eq!(
            sum_tree
                .get_feature_type(
                    b"b",
                    true,
                    Some(&Element::value_defined_cost_for_serialized_value),
                    grove_version
                )
                .unwrap()
                .expect("node should exist"),
            Some(SummedMerkNode(10))
        );

        // Create new batch to use existing tree
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec(), b"key1".to_vec()],
            b"c".to_vec(),
            Element::new_sum_item(10),
        )];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("should apply batch");
        let transaction = db.start_transaction();

        let batch = StorageBatch::new();
        let sum_tree = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"key1"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");
        assert_eq!(
            sum_tree
                .get_feature_type(
                    b"c",
                    true,
                    None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                    grove_version
                )
                .unwrap()
                .expect("node should exist"),
            Some(SummedMerkNode(10))
        );
        assert_eq!(
            sum_tree.aggregate_data().expect("expected to get sum"),
            AggregateData::Sum(20)
        );

        // Test propagation
        // Add a new sum tree with its own sum items, should affect sum of original
        // tree
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"key1".to_vec()],
                b"d".to_vec(),
                Element::empty_sum_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"key1".to_vec(), b"d".to_vec()],
                b"first".to_vec(),
                Element::new_sum_item(4),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"key1".to_vec(), b"d".to_vec()],
                b"second".to_vec(),
                Element::new_item(vec![4]),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"key1".to_vec()],
                b"e".to_vec(),
                Element::empty_sum_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"key1".to_vec(), b"e".to_vec()],
                b"first".to_vec(),
                Element::new_sum_item(12),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"key1".to_vec(), b"e".to_vec()],
                b"second".to_vec(),
                Element::new_item(vec![4]),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"key1".to_vec(), b"e".to_vec()],
                b"third".to_vec(),
                Element::empty_sum_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![
                    TEST_LEAF.to_vec(),
                    b"key1".to_vec(),
                    b"e".to_vec(),
                    b"third".to_vec(),
                ],
                b"a".to_vec(),
                Element::new_sum_item(5),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![
                    TEST_LEAF.to_vec(),
                    b"key1".to_vec(),
                    b"e".to_vec(),
                    b"third".to_vec(),
                ],
                b"b".to_vec(),
                Element::new_item(vec![5]),
            ),
        ];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("should apply batch");
        let transaction = db.start_transaction();

        let batch = StorageBatch::new();
        let sum_tree = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"key1"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");
        assert_eq!(
            sum_tree.aggregate_data().expect("expected to get sum"),
            AggregateData::Sum(41)
        );
    }

    #[test]
    fn test_big_sum_tree_with_batches() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"key1".to_vec(),
                Element::empty_big_sum_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"key1".to_vec()],
                b"a".to_vec(),
                Element::new_item(vec![214]),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"key1".to_vec()],
                b"b".to_vec(),
                Element::new_sum_item(10),
            ),
        ];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("should apply batch");
        let transaction = db.start_transaction();

        let batch = StorageBatch::new();
        let sum_tree = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"key1"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");

        assert_eq!(
            sum_tree
                .get_feature_type(
                    b"a",
                    true,
                    Some(&Element::value_defined_cost_for_serialized_value),
                    grove_version
                )
                .unwrap()
                .expect("node should exist"),
            Some(BigSummedMerkNode(0))
        );
        assert_eq!(
            sum_tree
                .get_feature_type(
                    b"b",
                    true,
                    Some(&Element::value_defined_cost_for_serialized_value),
                    grove_version
                )
                .unwrap()
                .expect("node should exist"),
            Some(BigSummedMerkNode(10))
        );

        // Create new batch to use existing tree
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec(), b"key1".to_vec()],
            b"c".to_vec(),
            Element::new_sum_item(10),
        )];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("should apply batch");
        let transaction = db.start_transaction();

        let batch = StorageBatch::new();
        let sum_tree = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"key1"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");
        assert_eq!(
            sum_tree
                .get_feature_type(
                    b"c",
                    true,
                    None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                    grove_version
                )
                .unwrap()
                .expect("node should exist"),
            Some(BigSummedMerkNode(10))
        );
        assert_eq!(
            sum_tree.aggregate_data().expect("expected to get sum"),
            AggregateData::BigSum(20)
        );

        // Test propagation
        // Add a new sum tree with its own sum items, should affect sum of original
        // tree
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"key1".to_vec()],
                b"d".to_vec(),
                Element::empty_sum_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"key1".to_vec(), b"d".to_vec()],
                b"first".to_vec(),
                Element::new_sum_item(4),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"key1".to_vec(), b"d".to_vec()],
                b"second".to_vec(),
                Element::new_item(vec![4]),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"key1".to_vec()],
                b"e".to_vec(),
                Element::empty_sum_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"key1".to_vec(), b"e".to_vec()],
                b"first".to_vec(),
                Element::new_sum_item(12),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"key1".to_vec(), b"e".to_vec()],
                b"second".to_vec(),
                Element::new_item(vec![4]),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"key1".to_vec(), b"e".to_vec()],
                b"third".to_vec(),
                Element::empty_sum_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![
                    TEST_LEAF.to_vec(),
                    b"key1".to_vec(),
                    b"e".to_vec(),
                    b"third".to_vec(),
                ],
                b"a".to_vec(),
                Element::new_sum_item(5),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![
                    TEST_LEAF.to_vec(),
                    b"key1".to_vec(),
                    b"e".to_vec(),
                    b"third".to_vec(),
                ],
                b"b".to_vec(),
                Element::new_item(vec![5]),
            ),
        ];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("should apply batch");
        let transaction = db.start_transaction();

        let batch = StorageBatch::new();
        let sum_tree = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"key1"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");
        assert_eq!(
            sum_tree.aggregate_data().expect("expected to get sum"),
            AggregateData::BigSum(41)
        );
    }

    #[test]
    fn test_verify_query_get_parent_tree_info() {
        let grove_version = GroveVersion::latest();
        let tmp_dir = TempDir::new().unwrap();
        let db = GroveDb::open(tmp_dir.path()).unwrap();

        // Create structure: root -> "parent" (SumTree) -> "child" (SumItem(1))
        db.insert(
            &[] as &[&[u8]],
            b"parent",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum tree");

        db.insert(
            [b"parent".as_ref()].as_ref(),
            b"child",
            Element::new_sum_item(1),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum item");

        let path_query = PathQuery::new_single_key(vec![b"parent".to_vec()], b"child".to_vec());

        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should generate proof");

        let (_root_hash, parent, result_set) =
            GroveDb::verify_subset_query_get_parent_tree_info(&proof, &path_query, grove_version)
                .expect("should verify proof");

        assert_eq!(parent, SummedMerkNode(1));
        assert_eq!(result_set.len(), 1);

        let (_path, _key, maybe_element) = &result_set[0];
        let element = maybe_element.as_ref().expect("expected Some(element)");
        assert!(
            matches!(element, Element::SumItem(1, _)),
            "expected SumItem(1), got: {:?}",
            element
        );
    }

    #[test]
    fn test_count_sum_tree_sum_propagates_to_sum_tree_parent() {
        // Test that sums from CountSumTree propagate correctly to SumTree parent
        //
        // Tree structure:
        //   TEST_LEAF
        //     └── sum_tree_parent (SumTree)
        //           ├── count_sum_child (CountSumTree)
        //           │     ├── sum_item_1 = 100
        //           │     ├── sum_item_2 = 200
        //           │     └── sum_item_3 = -50
        //           └── direct_sum_item = 25
        //
        // Expected: sum_tree_parent sum = 100 + 200 + (-50) + 25 = 275

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Create parent SumTree
        db.insert(
            [TEST_LEAF].as_ref(),
            b"sum_tree_parent",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum_tree_parent");

        // Create child CountSumTree inside the SumTree
        db.insert(
            [TEST_LEAF, b"sum_tree_parent"].as_ref(),
            b"count_sum_child",
            Element::new_count_sum_tree(None),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert count_sum_child");

        // Insert sum items into CountSumTree
        db.insert(
            [TEST_LEAF, b"sum_tree_parent", b"count_sum_child"].as_ref(),
            b"sum_item_1",
            Element::new_sum_item(100),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum_item_1");

        db.insert(
            [TEST_LEAF, b"sum_tree_parent", b"count_sum_child"].as_ref(),
            b"sum_item_2",
            Element::new_sum_item(200),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum_item_2");

        db.insert(
            [TEST_LEAF, b"sum_tree_parent", b"count_sum_child"].as_ref(),
            b"sum_item_3",
            Element::new_sum_item(-50),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum_item_3");

        // Insert a direct sum item into the parent SumTree
        db.insert(
            [TEST_LEAF, b"sum_tree_parent"].as_ref(),
            b"direct_sum_item",
            Element::new_sum_item(25),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert direct_sum_item");

        // Verify CountSumTree child has correct count and sum
        let batch = StorageBatch::new();
        let transaction = db.start_transaction();

        let child_merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"sum_tree_parent", b"count_sum_child"]
                    .as_ref()
                    .into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open count_sum_child");

        let child_aggregate = child_merk
            .aggregate_data()
            .expect("expected to get aggregate data");
        assert_eq!(
            child_aggregate,
            AggregateData::CountAndSum(3, 250),
            "CountSumTree should have count=3, sum=250"
        );

        // Verify parent SumTree has the propagated sum
        let parent = db
            .get(
                [TEST_LEAF].as_ref(),
                b"sum_tree_parent",
                None,
                grove_version,
            )
            .unwrap()
            .expect("should get sum_tree_parent");

        assert_eq!(
            parent.sum_value_or_default(),
            275, // 100 + 200 + (-50) + 25 = 275
            "SumTree parent should have sum=275 (propagated from CountSumTree + direct item)"
        );
    }

    #[test]
    fn test_count_sum_tree_sum_propagates_to_big_sum_tree_parent() {
        // Test that sums from CountSumTree propagate correctly to BigSumTree parent
        // Uses separate CountSumTrees each with a large value to demonstrate
        // BigSumTree can hold sums exceeding i64::MAX
        //
        // Tree structure:
        //   TEST_LEAF
        //     └── big_sum_tree_parent (BigSumTree)
        //           ├── count_sum_child_1 (CountSumTree)
        //           │     └── sum_item_a = i64::MAX - 100
        //           ├── count_sum_child_2 (CountSumTree)
        //           │     └── sum_item_b = i64::MAX - 200
        //           └── direct_sum_item = 1000
        //
        // Expected: big_sum_tree sum = (i64::MAX-100) + (i64::MAX-200) + 1000
        //         which exceeds i64::MAX, demonstrating BigSumTree capability

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Create parent BigSumTree
        db.insert(
            [TEST_LEAF].as_ref(),
            b"big_sum_tree_parent",
            Element::empty_big_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert big_sum_tree_parent");

        // Create first child CountSumTree
        db.insert(
            [TEST_LEAF, b"big_sum_tree_parent"].as_ref(),
            b"count_sum_child_1",
            Element::new_count_sum_tree(None),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert count_sum_child_1");

        // Create second child CountSumTree
        db.insert(
            [TEST_LEAF, b"big_sum_tree_parent"].as_ref(),
            b"count_sum_child_2",
            Element::new_count_sum_tree(None),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert count_sum_child_2");

        // Insert sum item into first CountSumTree (large value)
        db.insert(
            [TEST_LEAF, b"big_sum_tree_parent", b"count_sum_child_1"].as_ref(),
            b"sum_item_a",
            Element::new_sum_item(SumValue::MAX - 100),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum_item_a");

        // Insert sum item into second CountSumTree (large value)
        db.insert(
            [TEST_LEAF, b"big_sum_tree_parent", b"count_sum_child_2"].as_ref(),
            b"sum_item_b",
            Element::new_sum_item(SumValue::MAX - 200),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum_item_b");

        // Insert a direct sum item into the parent BigSumTree
        db.insert(
            [TEST_LEAF, b"big_sum_tree_parent"].as_ref(),
            b"direct_sum_item",
            Element::new_sum_item(1000),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert direct_sum_item");

        // Verify first CountSumTree child
        let batch = StorageBatch::new();
        let transaction = db.start_transaction();

        let child1_merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"big_sum_tree_parent", b"count_sum_child_1"]
                    .as_ref()
                    .into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open count_sum_child_1");

        let child1_aggregate = child1_merk
            .aggregate_data()
            .expect("expected to get aggregate data");
        assert_eq!(
            child1_aggregate,
            AggregateData::CountAndSum(1, SumValue::MAX - 100),
            "CountSumTree child_1 should have count=1, sum=MAX-100"
        );

        // Verify second CountSumTree child
        let child2_merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"big_sum_tree_parent", b"count_sum_child_2"]
                    .as_ref()
                    .into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open count_sum_child_2");

        let child2_aggregate = child2_merk
            .aggregate_data()
            .expect("expected to get aggregate data");
        assert_eq!(
            child2_aggregate,
            AggregateData::CountAndSum(1, SumValue::MAX - 200),
            "CountSumTree child_2 should have count=1, sum=MAX-200"
        );

        // Verify parent BigSumTree has the propagated sum
        let parent = db
            .get(
                [TEST_LEAF].as_ref(),
                b"big_sum_tree_parent",
                None,
                grove_version,
            )
            .unwrap()
            .expect("should get big_sum_tree_parent");

        // Total exceeds i64::MAX, demonstrating BigSumTree capability
        let expected_total: i128 =
            (SumValue::MAX - 100) as i128 + (SumValue::MAX - 200) as i128 + 1000i128;

        assert_eq!(
            parent.big_sum_value_or_default(),
            expected_total,
            "BigSumTree parent should have correct propagated sum from CountSumTrees"
        );

        // Verify the sum actually exceeds i64::MAX
        assert!(
            expected_total > i64::MAX as i128,
            "Expected total should exceed i64::MAX to demonstrate BigSumTree"
        );
    }

    #[test]
    fn test_sum_item_proof_generation_and_verification() {
        // Regression test: SumItem was silently dropped during proof generation
        // because the match in prove_subqueries only handled Element::Item,
        // causing SumItem to fall through to `_ => continue`.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Create a SumTree and insert SumItems
        db.insert(
            [TEST_LEAF].as_ref(),
            b"sum_tree",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum tree");

        db.insert(
            [TEST_LEAF, b"sum_tree"].as_ref(),
            b"key1",
            Element::new_sum_item(42),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum item 1");

        db.insert(
            [TEST_LEAF, b"sum_tree"].as_ref(),
            b"key2",
            Element::new_sum_item(-10),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum item 2");

        // Prove a query for the sum items
        let mut query = Query::new();
        query.insert_all();
        let path_query =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"sum_tree".to_vec()], query);

        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should generate proof for sum items");

        let (root_hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query, grove_version)
            .expect("should verify proof containing sum items");

        assert_eq!(
            root_hash,
            db.grove_db.root_hash(None, grove_version).unwrap().unwrap()
        );
        // Both SumItems must appear in the result set
        assert_eq!(
            result_set.len(),
            2,
            "both sum items should be in the proof result set"
        );

        let elem1 = Element::deserialize(&result_set[0].value, grove_version)
            .expect("should deserialize first element");
        let elem2 = Element::deserialize(&result_set[1].value, grove_version)
            .expect("should deserialize second element");
        assert!(matches!(elem1, Element::SumItem(42, _)));
        assert!(matches!(elem2, Element::SumItem(-10, _)));
    }

    #[test]
    fn test_item_with_sum_item_proof_generation_and_verification() {
        // Regression test: ItemWithSumItem was silently dropped during proof
        // generation for the same reason as SumItem.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Create a SumTree and insert ItemWithSumItem elements
        db.insert(
            [TEST_LEAF].as_ref(),
            b"sum_tree",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum tree");

        db.insert(
            [TEST_LEAF, b"sum_tree"].as_ref(),
            b"key1",
            Element::new_item_with_sum_item(b"payload1".to_vec(), 100),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item with sum item 1");

        db.insert(
            [TEST_LEAF, b"sum_tree"].as_ref(),
            b"key2",
            Element::new_item_with_sum_item(b"payload2".to_vec(), 200),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item with sum item 2");

        // Prove a query for the items
        let mut query = Query::new();
        query.insert_all();
        let path_query =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"sum_tree".to_vec()], query);

        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should generate proof for item with sum items");

        let (root_hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query, grove_version)
            .expect("should verify proof containing item with sum items");

        assert_eq!(
            root_hash,
            db.grove_db.root_hash(None, grove_version).unwrap().unwrap()
        );
        // Both ItemWithSumItem elements must appear in the result set
        assert_eq!(
            result_set.len(),
            2,
            "both item-with-sum-items should be in the proof result set"
        );

        let elem1 = Element::deserialize(&result_set[0].value, grove_version)
            .expect("should deserialize first element");
        let elem2 = Element::deserialize(&result_set[1].value, grove_version)
            .expect("should deserialize second element");
        assert!(matches!(elem1, Element::ItemWithSumItem(_, 100, _)));
        assert!(matches!(elem2, Element::ItemWithSumItem(_, 200, _)));
    }
}
