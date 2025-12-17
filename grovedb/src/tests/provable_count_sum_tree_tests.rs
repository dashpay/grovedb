//! ProvableCountSumTree tests
//!
//! ProvableCountSumTree combines the functionality of ProvableCountTree and
//! CountSumTree:
//! - The COUNT is included in the cryptographic hash (like ProvableCountTree)
//! - The SUM is tracked but NOT included in the hash (for query purposes)
//!
//! This allows for cryptographic proofs of element count while also tracking
//! sums.

#[cfg(test)]
mod provable_count_sum_tree_tests {
    use grovedb_merk::{
        proofs::Query,
        tree::{kv::ValueDefinedCostType, AggregateData},
        TreeFeatureType,
    };
    use grovedb_storage::StorageBatch;
    use grovedb_version::version::GroveVersion;

    use crate::{
        batch::QualifiedGroveDbOp,
        tests::{make_test_grovedb, TEST_LEAF},
        Element, GroveDb, PathQuery,
    };

    #[test]
    fn test_provable_count_sum_tree_behaves_like_regular_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert a ProvableCountSumTree
        db.insert(
            [TEST_LEAF].as_ref(),
            b"provable_count_sum_key",
            Element::new_provable_count_sum_tree(None),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert ProvableCountSumTree");

        // Fetch the ProvableCountSumTree
        let provable_count_sum_tree = db
            .get(
                [TEST_LEAF].as_ref(),
                b"provable_count_sum_key",
                None,
                grove_version,
            )
            .unwrap()
            .expect("should get ProvableCountSumTree");
        assert!(matches!(
            provable_count_sum_tree,
            Element::ProvableCountSumTree(..)
        ));

        // Insert items into the ProvableCountSumTree
        db.insert(
            [TEST_LEAF, b"provable_count_sum_key"].as_ref(),
            b"item1",
            Element::new_item(vec![1]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item1");

        db.insert(
            [TEST_LEAF, b"provable_count_sum_key"].as_ref(),
            b"item2",
            Element::new_sum_item(3),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item2");

        db.insert(
            [TEST_LEAF, b"provable_count_sum_key"].as_ref(),
            b"item3",
            Element::new_sum_item(5),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item3");

        // Test proper item retrieval
        let item1 = db
            .get(
                [TEST_LEAF, b"provable_count_sum_key"].as_ref(),
                b"item1",
                None,
                grove_version,
            )
            .unwrap()
            .expect("should get item1");
        assert_eq!(item1, Element::new_item(vec![1]));

        let item2 = db
            .get(
                [TEST_LEAF, b"provable_count_sum_key"].as_ref(),
                b"item2",
                None,
                grove_version,
            )
            .unwrap()
            .expect("should get item2");
        assert_eq!(item2, Element::new_sum_item(3));

        let item3 = db
            .get(
                [TEST_LEAF, b"provable_count_sum_key"].as_ref(),
                b"item3",
                None,
                grove_version,
            )
            .unwrap()
            .expect("should get item3");
        assert_eq!(item3, Element::new_sum_item(5));

        // Test aggregate data (count and sum)
        let batch = StorageBatch::new();
        let transaction = db.start_transaction();

        let merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"provable_count_sum_key"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open ProvableCountSumTree");

        let aggregate_data = merk
            .aggregate_data()
            .expect("expected to get aggregate data");

        // Should have ProvableCountAndSum aggregate data: 3 items with sum of 8 (3 + 5)
        assert_eq!(aggregate_data, AggregateData::ProvableCountAndSum(3, 8));
    }

    #[test]
    fn test_provable_count_sum_tree_feature_types() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert a ProvableCountSumTree
        db.insert(
            [TEST_LEAF].as_ref(),
            b"feature_test",
            Element::new_provable_count_sum_tree(None),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert ProvableCountSumTree");

        // Insert items
        db.insert(
            [TEST_LEAF, b"feature_test"].as_ref(),
            b"regular_item",
            Element::new_item(vec![10]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert regular_item");

        db.insert(
            [TEST_LEAF, b"feature_test"].as_ref(),
            b"sum_item",
            Element::new_sum_item(20),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum_item");

        // Open merk and check feature types
        let batch = StorageBatch::new();
        let transaction = db.start_transaction();

        let merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"feature_test"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open ProvableCountSumTree");

        // Verify feature types - should be ProvableCountedSummedMerkNode
        let feature_type_regular = merk
            .get_feature_type(
                b"regular_item",
                true,
                None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                grove_version,
            )
            .unwrap()
            .expect("node should exist")
            .expect("expected feature type");
        assert_eq!(
            feature_type_regular,
            TreeFeatureType::ProvableCountedSummedMerkNode(1, 0)
        );

        let feature_type_sum = merk
            .get_feature_type(
                b"sum_item",
                true,
                None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                grove_version,
            )
            .unwrap()
            .expect("node should exist")
            .expect("expected feature type");
        assert_eq!(
            feature_type_sum,
            TreeFeatureType::ProvableCountedSummedMerkNode(1, 20)
        );
    }

    #[test]
    fn test_provable_count_sum_tree_proof_generation_and_verification() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Create a ProvableCountSumTree - following the same pattern as
        // ProvableCountTree test
        db.insert(
            &[] as &[&[u8]],
            b"test_tree",
            Element::empty_provable_count_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");

        // Insert an item
        db.insert(
            &[b"test_tree"],
            b"key1",
            Element::new_item(b"value1".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        // Query for the item
        let mut query = Query::new();
        query.insert_key(b"key1".to_vec());
        let path_query = PathQuery::new_unsized(vec![b"test_tree".to_vec()], query);

        // Generate proof
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should generate proof");

        // Verify proof
        let (root_hash, proved_values) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version)
                .expect("should verify proof");

        // Check root hash matches
        let actual_root_hash = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get root hash");

        assert_eq!(root_hash, actual_root_hash, "Root hash should match");
        assert_eq!(proved_values.len(), 1, "Should have 1 proved value");
        assert_eq!(proved_values[0].key, b"key1");
    }

    #[test]
    fn test_provable_count_sum_tree_with_batches() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Prepare a batch of operations
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"batch_provable_count_sum".to_vec(),
                Element::new_provable_count_sum_tree(None),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"batch_provable_count_sum".to_vec()],
                b"a".to_vec(),
                Element::new_item(vec![10]),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"batch_provable_count_sum".to_vec()],
                b"b".to_vec(),
                Element::new_sum_item(20),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"batch_provable_count_sum".to_vec()],
                b"c".to_vec(),
                Element::new_sum_item(30),
            ),
        ];

        // Apply the batch
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("should apply batch");

        // Open the ProvableCountSumTree and verify aggregate data
        let batch = StorageBatch::new();
        let transaction = db.start_transaction();

        let merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"batch_provable_count_sum"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open ProvableCountSumTree");

        let aggregate_data = merk
            .aggregate_data()
            .expect("expected to get aggregate data");
        // count=3, sum=20+30=50
        assert_eq!(aggregate_data, AggregateData::ProvableCountAndSum(3, 50));
    }

    #[test]
    fn test_provable_count_sum_tree_propagation() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert a parent ProvableCountSumTree
        db.insert(
            [TEST_LEAF].as_ref(),
            b"parent_provable",
            Element::new_provable_count_sum_tree(None),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert parent ProvableCountSumTree");

        // Insert a child ProvableCountSumTree within the parent
        db.insert(
            [TEST_LEAF, b"parent_provable"].as_ref(),
            b"child_provable",
            Element::new_provable_count_sum_tree(None),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert child ProvableCountSumTree");

        // Insert items into the child ProvableCountSumTree
        db.insert(
            [TEST_LEAF, b"parent_provable", b"child_provable"].as_ref(),
            b"item1",
            Element::new_item(vec![5]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item1 into child");

        db.insert(
            [TEST_LEAF, b"parent_provable", b"child_provable"].as_ref(),
            b"item2",
            Element::new_sum_item(15),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item2 into child");

        // Verify aggregate data of child
        let batch = StorageBatch::new();
        let transaction = db.start_transaction();

        let child_merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"parent_provable", b"child_provable"]
                    .as_ref()
                    .into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open child ProvableCountSumTree");

        let child_aggregate = child_merk
            .aggregate_data()
            .expect("expected to get aggregate data");
        assert_eq!(child_aggregate, AggregateData::ProvableCountAndSum(2, 15));

        // Verify aggregate data of parent (should include child tree's contribution)
        let parent_batch = StorageBatch::new();
        let parent_merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"parent_provable"].as_ref().into(),
                &transaction,
                Some(&parent_batch),
                grove_version,
            )
            .unwrap()
            .expect("should open parent ProvableCountSumTree");

        let parent_aggregate = parent_merk
            .aggregate_data()
            .expect("expected to get aggregate data");
        // Parent sees the child tree as 1 element with sum from child tree
        assert_eq!(parent_aggregate, AggregateData::ProvableCountAndSum(2, 15));
    }

    #[test]
    fn test_provable_count_sum_tree_constructors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Test empty_provable_count_sum_tree
        db.insert(
            [TEST_LEAF].as_ref(),
            b"empty_tree",
            Element::empty_provable_count_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert empty ProvableCountSumTree");

        // Test empty_provable_count_sum_tree_with_flags
        db.insert(
            [TEST_LEAF].as_ref(),
            b"flagged_tree",
            Element::empty_provable_count_sum_tree_with_flags(Some(vec![1, 2, 3])),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert flagged ProvableCountSumTree");

        // Test new_provable_count_sum_tree
        db.insert(
            [TEST_LEAF].as_ref(),
            b"new_tree",
            Element::new_provable_count_sum_tree(None),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert new ProvableCountSumTree");

        // Test new_provable_count_sum_tree_with_flags
        db.insert(
            [TEST_LEAF].as_ref(),
            b"new_flagged_tree",
            Element::new_provable_count_sum_tree_with_flags(None, Some(vec![4, 5, 6])),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert new flagged ProvableCountSumTree");

        // Verify all trees were inserted correctly
        let empty_tree = db
            .get([TEST_LEAF].as_ref(), b"empty_tree", None, grove_version)
            .unwrap()
            .expect("should get empty_tree");
        assert!(matches!(empty_tree, Element::ProvableCountSumTree(..)));

        let flagged_tree = db
            .get([TEST_LEAF].as_ref(), b"flagged_tree", None, grove_version)
            .unwrap()
            .expect("should get flagged_tree");
        assert!(matches!(flagged_tree, Element::ProvableCountSumTree(..)));
        assert_eq!(flagged_tree.get_flags(), &Some(vec![1, 2, 3]));
    }

    #[test]
    fn test_provable_count_sum_tree_vs_count_sum_tree_aggregate_difference() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert a regular CountSumTree
        db.insert(
            [TEST_LEAF].as_ref(),
            b"regular_count_sum",
            Element::new_count_sum_tree(None),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert CountSumTree");

        // Insert a ProvableCountSumTree
        db.insert(
            [TEST_LEAF].as_ref(),
            b"provable_count_sum",
            Element::new_provable_count_sum_tree(None),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert ProvableCountSumTree");

        // Insert same items into both
        for tree_key in [
            b"regular_count_sum".as_slice(),
            b"provable_count_sum".as_slice(),
        ] {
            db.insert(
                [TEST_LEAF, tree_key].as_ref(),
                b"item1",
                Element::new_sum_item(10),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert item1");

            db.insert(
                [TEST_LEAF, tree_key].as_ref(),
                b"item2",
                Element::new_sum_item(20),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert item2");
        }

        let batch = StorageBatch::new();
        let transaction = db.start_transaction();

        // Check regular CountSumTree aggregate
        let regular_merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"regular_count_sum"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open CountSumTree");

        let regular_aggregate = regular_merk
            .aggregate_data()
            .expect("expected to get aggregate data");
        assert_eq!(regular_aggregate, AggregateData::CountAndSum(2, 30));

        // Check ProvableCountSumTree aggregate
        let provable_batch = StorageBatch::new();
        let provable_merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"provable_count_sum"].as_ref().into(),
                &transaction,
                Some(&provable_batch),
                grove_version,
            )
            .unwrap()
            .expect("should open ProvableCountSumTree");

        let provable_aggregate = provable_merk
            .aggregate_data()
            .expect("expected to get aggregate data");
        // Both should have same count and sum values, but different types
        assert_eq!(
            provable_aggregate,
            AggregateData::ProvableCountAndSum(2, 30)
        );
    }

    #[test]
    fn test_provable_count_sum_tree_helper_methods() {
        // Test is_provable_count_sum_tree helper
        let tree = Element::new_provable_count_sum_tree(None);
        assert!(tree.is_any_tree());

        // Test count_sum_value_or_default
        let tree_with_values =
            Element::new_provable_count_sum_tree_with_flags_and_sum_and_count_value(
                None, 5, 100, None,
            );
        assert_eq!(tree_with_values.count_sum_value_or_default(), (5, 100));
    }

    // ==================== EDGE CASE TESTS ====================

    #[test]
    fn test_provable_count_sum_tree_empty_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert an empty ProvableCountSumTree (no items inside)
        db.insert(
            [TEST_LEAF].as_ref(),
            b"empty_provable",
            Element::empty_provable_count_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert empty ProvableCountSumTree");

        // Verify we can retrieve it
        let retrieved = db
            .get([TEST_LEAF].as_ref(), b"empty_provable", None, grove_version)
            .unwrap()
            .expect("should get empty tree");
        assert!(matches!(
            retrieved,
            Element::ProvableCountSumTree(None, 0, 0, None)
        ));

        // Verify aggregate data for empty tree
        let batch = StorageBatch::new();
        let transaction = db.start_transaction();

        let merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"empty_provable"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open empty ProvableCountSumTree");

        let aggregate_data = merk
            .aggregate_data()
            .expect("expected to get aggregate data");
        // Empty tree (no root node) returns NoAggregateData
        assert_eq!(aggregate_data, AggregateData::NoAggregateData);
    }

    #[test]
    fn test_provable_count_sum_tree_empty_tree_proof() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert an empty ProvableCountSumTree
        db.insert(
            &[] as &[&[u8]],
            b"empty_tree",
            Element::empty_provable_count_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert empty tree");

        // Query for a non-existent key (absence proof)
        let mut query = Query::new();
        query.insert_key(b"nonexistent".to_vec());
        let path_query = PathQuery::new_unsized(vec![b"empty_tree".to_vec()], query);

        // Generate proof for absence
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should generate absence proof");

        // Verify proof
        let (root_hash, proved_values) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version)
                .expect("should verify absence proof");

        // Check root hash matches
        let actual_root_hash = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get root hash");

        assert_eq!(root_hash, actual_root_hash, "Root hash should match");
        assert_eq!(
            proved_values.len(),
            0,
            "Should have no proved values for absence"
        );
    }

    #[test]
    fn test_provable_count_sum_tree_with_negative_sums() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert a ProvableCountSumTree
        db.insert(
            [TEST_LEAF].as_ref(),
            b"negative_sum_tree",
            Element::new_provable_count_sum_tree(None),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");

        // Insert items with negative sum values
        db.insert(
            [TEST_LEAF, b"negative_sum_tree"].as_ref(),
            b"positive",
            Element::new_sum_item(100),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert positive sum item");

        db.insert(
            [TEST_LEAF, b"negative_sum_tree"].as_ref(),
            b"negative",
            Element::new_sum_item(-150),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert negative sum item");

        // Verify aggregate data
        let batch = StorageBatch::new();
        let transaction = db.start_transaction();

        let merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"negative_sum_tree"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");

        let aggregate_data = merk
            .aggregate_data()
            .expect("expected to get aggregate data");
        // count=2, sum=100+(-150)=-50
        assert_eq!(aggregate_data, AggregateData::ProvableCountAndSum(2, -50));
    }

    #[test]
    fn test_provable_count_sum_tree_single_item() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert a ProvableCountSumTree with a single item
        db.insert(
            [TEST_LEAF].as_ref(),
            b"single_item_tree",
            Element::new_provable_count_sum_tree(None),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");

        db.insert(
            [TEST_LEAF, b"single_item_tree"].as_ref(),
            b"only_item",
            Element::new_sum_item(42),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert single item");

        // Verify aggregate data
        let batch = StorageBatch::new();
        let transaction = db.start_transaction();

        let merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"single_item_tree"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");

        let aggregate_data = merk
            .aggregate_data()
            .expect("expected to get aggregate data");
        assert_eq!(aggregate_data, AggregateData::ProvableCountAndSum(1, 42));

        // Also verify proof works with single item
        let mut query = Query::new();
        query.insert_key(b"only_item".to_vec());
        let path_query = PathQuery::new_unsized(
            vec![TEST_LEAF.to_vec(), b"single_item_tree".to_vec()],
            query,
        );

        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should generate proof");

        let (root_hash, proved_values) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version)
                .expect("should verify proof");

        let actual_root_hash = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get root hash");

        assert_eq!(root_hash, actual_root_hash);
        assert_eq!(proved_values.len(), 1);
    }

    // ==================== COMPLEX NESTED TREE TESTS ====================

    #[test]
    fn test_provable_count_sum_tree_deeply_nested() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Create a deeply nested structure:
        // TEST_LEAF -> level1 (ProvableCountSumTree) -> level2 (ProvableCountSumTree)
        // -> level3 (ProvableCountSumTree) -> items

        db.insert(
            [TEST_LEAF].as_ref(),
            b"level1",
            Element::new_provable_count_sum_tree(None),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert level1");

        db.insert(
            [TEST_LEAF, b"level1"].as_ref(),
            b"level2",
            Element::new_provable_count_sum_tree(None),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert level2");

        db.insert(
            [TEST_LEAF, b"level1", b"level2"].as_ref(),
            b"level3",
            Element::new_provable_count_sum_tree(None),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert level3");

        // Insert items at deepest level
        db.insert(
            [TEST_LEAF, b"level1", b"level2", b"level3"].as_ref(),
            b"item1",
            Element::new_sum_item(10),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item1");

        db.insert(
            [TEST_LEAF, b"level1", b"level2", b"level3"].as_ref(),
            b"item2",
            Element::new_sum_item(20),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item2");

        // Verify aggregate at deepest level
        let batch = StorageBatch::new();
        let transaction = db.start_transaction();

        let level3_merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"level1", b"level2", b"level3"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open level3");

        let level3_aggregate = level3_merk
            .aggregate_data()
            .expect("expected to get aggregate data");
        assert_eq!(level3_aggregate, AggregateData::ProvableCountAndSum(2, 30));

        // Query items at deepest level and verify proof
        let mut query = Query::new();
        query.insert_all();
        let path_query = PathQuery::new_unsized(
            vec![
                TEST_LEAF.to_vec(),
                b"level1".to_vec(),
                b"level2".to_vec(),
                b"level3".to_vec(),
            ],
            query,
        );

        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should generate proof");

        let (root_hash, proved_values) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version)
                .expect("should verify proof");

        let actual_root_hash = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get root hash");

        assert_eq!(root_hash, actual_root_hash);
        assert_eq!(proved_values.len(), 2);
    }

    #[test]
    fn test_provable_count_sum_tree_mixed_tree_types_nested() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Create a mixed structure:
        // TEST_LEAF -> regular_tree (Tree) -> provable_sum (ProvableCountSumTree) ->
        // items

        db.insert(
            [TEST_LEAF].as_ref(),
            b"regular_tree",
            Element::new_tree(None),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert regular tree");

        db.insert(
            [TEST_LEAF, b"regular_tree"].as_ref(),
            b"provable_sum",
            Element::new_provable_count_sum_tree(None),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert provable count sum tree");

        // Insert items
        db.insert(
            [TEST_LEAF, b"regular_tree", b"provable_sum"].as_ref(),
            b"a",
            Element::new_sum_item(100),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item a");

        db.insert(
            [TEST_LEAF, b"regular_tree", b"provable_sum"].as_ref(),
            b"b",
            Element::new_item(vec![1, 2, 3]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item b");

        // Verify aggregate in provable tree
        let batch = StorageBatch::new();
        let transaction = db.start_transaction();

        let merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"regular_tree", b"provable_sum"]
                    .as_ref()
                    .into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open provable count sum tree");

        let aggregate_data = merk
            .aggregate_data()
            .expect("expected to get aggregate data");
        // count=2 (both items), sum=100 (only from sum item, regular item contributes
        // 0)
        assert_eq!(aggregate_data, AggregateData::ProvableCountAndSum(2, 100));
    }

    #[test]
    fn test_provable_count_sum_tree_sibling_trees() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Create sibling ProvableCountSumTrees at same level
        db.insert(
            [TEST_LEAF].as_ref(),
            b"sibling1",
            Element::new_provable_count_sum_tree(None),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sibling1");

        db.insert(
            [TEST_LEAF].as_ref(),
            b"sibling2",
            Element::new_provable_count_sum_tree(None),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sibling2");

        db.insert(
            [TEST_LEAF].as_ref(),
            b"sibling3",
            Element::new_provable_count_sum_tree(None),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sibling3");

        // Insert different items into each sibling
        db.insert(
            [TEST_LEAF, b"sibling1"].as_ref(),
            b"item",
            Element::new_sum_item(10),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert into sibling1");

        db.insert(
            [TEST_LEAF, b"sibling2"].as_ref(),
            b"item",
            Element::new_sum_item(20),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert into sibling2");

        db.insert(
            [TEST_LEAF, b"sibling3"].as_ref(),
            b"item",
            Element::new_sum_item(30),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert into sibling3");

        // Verify each sibling has correct aggregate data
        let batch = StorageBatch::new();
        let transaction = db.start_transaction();

        for (sibling, expected_sum) in [(b"sibling1", 10), (b"sibling2", 20), (b"sibling3", 30)] {
            let merk = db
                .open_transactional_merk_at_path(
                    [TEST_LEAF, sibling.as_slice()].as_ref().into(),
                    &transaction,
                    Some(&batch),
                    grove_version,
                )
                .unwrap()
                .expect("should open sibling");

            let aggregate_data = merk
                .aggregate_data()
                .expect("expected to get aggregate data");
            assert_eq!(
                aggregate_data,
                AggregateData::ProvableCountAndSum(1, expected_sum)
            );
        }
    }

    #[test]
    fn test_provable_count_sum_tree_with_subtree_inside() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // ProvableCountSumTree containing another ProvableCountSumTree
        // The parent should see the child tree as one element
        db.insert(
            [TEST_LEAF].as_ref(),
            b"parent",
            Element::new_provable_count_sum_tree(None),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert parent");

        // Add a regular item to parent
        db.insert(
            [TEST_LEAF, b"parent"].as_ref(),
            b"regular_item",
            Element::new_sum_item(50),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert regular item");

        // Add a child ProvableCountSumTree to parent
        db.insert(
            [TEST_LEAF, b"parent"].as_ref(),
            b"child_tree",
            Element::new_provable_count_sum_tree(None),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert child tree");

        // Add items to child tree
        db.insert(
            [TEST_LEAF, b"parent", b"child_tree"].as_ref(),
            b"child_item1",
            Element::new_sum_item(100),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert child_item1");

        db.insert(
            [TEST_LEAF, b"parent", b"child_tree"].as_ref(),
            b"child_item2",
            Element::new_sum_item(200),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert child_item2");

        let batch = StorageBatch::new();
        let transaction = db.start_transaction();

        // Check child aggregate: count=2, sum=300
        let child_merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"parent", b"child_tree"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open child");

        let child_aggregate = child_merk
            .aggregate_data()
            .expect("expected to get aggregate data");
        assert_eq!(child_aggregate, AggregateData::ProvableCountAndSum(2, 300));

        // Check parent aggregate: count=2 (item + child_tree), sum=50+300=350
        let parent_batch = StorageBatch::new();
        let parent_merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"parent"].as_ref().into(),
                &transaction,
                Some(&parent_batch),
                grove_version,
            )
            .unwrap()
            .expect("should open parent");

        let parent_aggregate = parent_merk
            .aggregate_data()
            .expect("expected to get aggregate data");
        // Parent count is 3: 1 (regular_item) + 2 (propagated from child_tree's count)
        // Sum from regular_item (50) + child_tree's sum (300) = 350
        assert_eq!(parent_aggregate, AggregateData::ProvableCountAndSum(3, 350));
    }

    #[test]
    fn test_provable_count_sum_tree_delete_and_reinsert() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert tree with items
        db.insert(
            [TEST_LEAF].as_ref(),
            b"mutable_tree",
            Element::new_provable_count_sum_tree(None),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");

        db.insert(
            [TEST_LEAF, b"mutable_tree"].as_ref(),
            b"item1",
            Element::new_sum_item(100),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item1");

        db.insert(
            [TEST_LEAF, b"mutable_tree"].as_ref(),
            b"item2",
            Element::new_sum_item(200),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item2");

        // Verify initial state
        let batch = StorageBatch::new();
        let transaction = db.start_transaction();

        let merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"mutable_tree"].as_ref().into(),
                &transaction,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");

        let aggregate = merk.aggregate_data().expect("should get aggregate");
        assert_eq!(aggregate, AggregateData::ProvableCountAndSum(2, 300));
        drop(merk);
        drop(transaction);

        // Delete one item
        db.delete(
            [TEST_LEAF, b"mutable_tree"].as_ref(),
            b"item1",
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should delete item1");

        // Verify after delete
        let batch2 = StorageBatch::new();
        let transaction2 = db.start_transaction();

        let merk2 = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"mutable_tree"].as_ref().into(),
                &transaction2,
                Some(&batch2),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");

        let aggregate2 = merk2.aggregate_data().expect("should get aggregate");
        assert_eq!(aggregate2, AggregateData::ProvableCountAndSum(1, 200));
        drop(merk2);
        drop(transaction2);

        // Reinsert with different value
        db.insert(
            [TEST_LEAF, b"mutable_tree"].as_ref(),
            b"item1",
            Element::new_sum_item(500),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should reinsert item1");

        // Verify after reinsert
        let batch3 = StorageBatch::new();
        let transaction3 = db.start_transaction();

        let merk3 = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"mutable_tree"].as_ref().into(),
                &transaction3,
                Some(&batch3),
                grove_version,
            )
            .unwrap()
            .expect("should open tree");

        let aggregate3 = merk3.aggregate_data().expect("should get aggregate");
        assert_eq!(aggregate3, AggregateData::ProvableCountAndSum(2, 700));
    }
}
