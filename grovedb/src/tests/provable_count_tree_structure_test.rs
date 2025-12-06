//! Comprehensive test for ProvableCountTree with mixed subtree types
//!
//! Tree Structure:
//! ```text
//!                        [root]
//!                           |
//!                    ProvableCountTree("pcount")  [count=19]
//!                           |
//!     +--------+--------+--------+--------+--------+--------+--------+----------+--------+
//!     |        |        |        |        |        |        |        |          |        |
//!   Tree    Tree    SumTree  SumTree  SumTree  CountTree CountTree CountSumTree Items(7)
//!  "tree1" "tree2"  "sum1"   "sum2"   "sum3"   "cnt1"    "cnt2"    "cntsum1"
//!  [cnt=1] [cnt=1]  [cnt=1]  [cnt=1]  [cnt=1]  [cnt=3]   [cnt=1]   [cnt=3]      [cnt=7]
//!     |        |        |        |        |        |        |          |
//!   (empty) Item"x"  SumItem SumItem  (empty)  Item"a"  Item"b"   SumItem"y"
//!                     =100    =200             Item"c"            SumItem"z"
//!                                              Item"d"            SumItem"w"
//!                                                                  =50 =75 =25
//!
//! Items directly in pcount:
//!   - "item1" -> "value1"
//!   - "item2" -> "value2"
//!   - "item3" -> "value3"
//!   - "item4" -> "value4"
//!   - "item5" -> "value5"
//!   - "item6" -> "value6"
//!   - "item7" -> "value7"
//!
//! Count calculation for "pcount": 19
//!   - tree1: 1 (Tree counts as 1, contents don't propagate)
//!   - tree2: 1 (Tree counts as 1, contents don't propagate)
//!   - sum1: 1 (SumTree counts as 1, contents don't propagate)
//!   - sum2: 1 (SumTree counts as 1, contents don't propagate)
//!   - sum3: 1 (SumTree counts as 1, contents don't propagate)
//!   - cnt1: 3 (CountTree propagates its count: 3 items)
//!   - cnt2: 1 (CountTree propagates its count: 1 item)
//!   - cntsum1: 3 (CountSumTree propagates its count: 3 sum items)
//!   - item1-item7: 7
//!   = 1+1+1+1+1+3+1+3+7 = 19 total
//!
//! Note: CountTree, CountSumTree, and ProvableCountTree propagate their internal
//! counts upward. Regular Tree, SumTree, BigSumTree all count as 1 each.
//! ```

#[cfg(test)]
mod tests {
    use grovedb_merk::{
        proofs::{query::query_item::QueryItem, Query},
        TreeFeatureType,
    };
    use grovedb_version::version::GroveVersion;

    use crate::{tests::make_empty_grovedb, Element, GroveDb, PathQuery, SizedQuery};

    #[test]
    fn test_provable_count_tree_with_mixed_subtrees() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Keys as variables for easier path construction
        let pcount = b"pcount";
        let tree1 = b"tree1";
        let tree2 = b"tree2";
        let sum1 = b"sum1";
        let sum2 = b"sum2";
        let sum3 = b"sum3";
        let cnt1 = b"cnt1";
        let cnt2 = b"cnt2";
        let cntsum1 = b"cntsum1";

        // =================================================================
        // STEP 1: Create the ProvableCountTree at root
        // =================================================================
        db.insert(
            &[] as &[&[u8]],
            pcount,
            Element::empty_provable_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert provable count tree");

        // =================================================================
        // STEP 2: Insert 2 normal Trees
        // =================================================================

        // tree1 - empty tree
        db.insert(
            &[pcount.as_slice()],
            tree1,
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree1");

        // tree2 - tree with one item
        db.insert(
            &[pcount.as_slice()],
            tree2,
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree2");

        db.insert(
            &[pcount.as_slice(), tree2.as_slice()],
            b"x",
            Element::new_item(b"value_x".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item x in tree2");

        // =================================================================
        // STEP 3: Insert 3 SumTrees
        // =================================================================

        // sum1 - with a sum item of value 100
        db.insert(
            &[pcount.as_slice()],
            sum1,
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum1");

        db.insert(
            &[pcount.as_slice(), sum1.as_slice()],
            b"s1_item",
            Element::new_sum_item(100),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum item in sum1");

        // sum2 - with a sum item of value 200
        db.insert(
            &[pcount.as_slice()],
            sum2,
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum2");

        db.insert(
            &[pcount.as_slice(), sum2.as_slice()],
            b"s2_item",
            Element::new_sum_item(200),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum item in sum2");

        // sum3 - empty sum tree
        db.insert(
            &[pcount.as_slice()],
            sum3,
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum3");

        // =================================================================
        // STEP 4: Insert 2 CountTrees
        // =================================================================

        // cnt1 - with two items
        db.insert(
            &[pcount.as_slice()],
            cnt1,
            Element::empty_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert cnt1");

        db.insert(
            &[pcount.as_slice(), cnt1.as_slice()],
            b"a",
            Element::new_item(b"value_a".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item a in cnt1");

        db.insert(
            &[pcount.as_slice(), cnt1.as_slice()],
            b"c",
            Element::new_item(b"value_c".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item c in cnt1");

        db.insert(
            &[pcount.as_slice(), cnt1.as_slice()],
            b"d",
            Element::new_item(b"value_d".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item d in cnt1");

        // cnt2 - with one item
        db.insert(
            &[pcount.as_slice()],
            cnt2,
            Element::empty_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert cnt2");

        db.insert(
            &[pcount.as_slice(), cnt2.as_slice()],
            b"b",
            Element::new_item(b"value_b".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item b in cnt2");

        // =================================================================
        // STEP 5: Insert 1 CountSumTree
        // =================================================================

        // cntsum1 - with two sum items (values 50 and 75)
        db.insert(
            &[pcount.as_slice()],
            cntsum1,
            Element::empty_count_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert cntsum1");

        db.insert(
            &[pcount.as_slice(), cntsum1.as_slice()],
            b"y",
            Element::new_sum_item(50),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum item y in cntsum1");

        db.insert(
            &[pcount.as_slice(), cntsum1.as_slice()],
            b"z",
            Element::new_sum_item(75),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum item z in cntsum1");

        db.insert(
            &[pcount.as_slice(), cntsum1.as_slice()],
            b"w",
            Element::new_sum_item(25),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum item w in cntsum1");

        // =================================================================
        // STEP 6: Insert 7 Items directly in pcount
        // =================================================================
        for i in 1..=7u8 {
            let key = format!("item{}", i);
            let value = format!("value{}", i);
            db.insert(
                &[pcount.as_slice()],
                key.as_bytes(),
                Element::new_item(value.into_bytes()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .unwrap_or_else(|_| panic!("should insert {}", key));
        }

        // =================================================================
        // STEP 7: Verify the count by querying the ProvableCountTree
        // =================================================================

        // Get the ProvableCountTree element from root
        let pcount_element = db
            .get(&[] as &[&[u8]], pcount, None, grove_version)
            .unwrap()
            .expect("should get pcount");

        println!("ProvableCountTree element: {:?}", pcount_element);

        // The element should be a ProvableCountTree with count 19
        // Count = 1+1+1+1+1+3+1+3+7 = 19
        // (tree1=1, tree2=1, sum1=1, sum2=1, sum3=1, cnt1=3, cnt2=1, cntsum1=3, items=7)
        match &pcount_element {
            Element::ProvableCountTree(root_key, count, _) => {
                println!("  Root key: {:?}", root_key);
                println!("  Count: {}", count);

                assert_eq!(
                    *count, 19,
                    "ProvableCountTree should have count 19, got {}",
                    count
                );
            }
            _ => panic!("Expected ProvableCountTree, got {:?}", pcount_element),
        }

        // =================================================================
        // STEP 8: Query each element and verify via proof
        // =================================================================

        // List of all keys in pcount (sorted alphabetically as they appear in tree)
        // Note: tree1 and sum3 are empty trees, they may return 0 results in queries
        // but they still contribute to the count
        let all_keys: Vec<(&[u8], bool)> = vec![
            (b"cnt1", true),
            (b"cnt2", true),
            (b"cntsum1", true),
            (b"item1", true),
            (b"item2", true),
            (b"item3", true),
            (b"item4", true),
            (b"item5", true),
            (b"item6", true),
            (b"item7", true),
            (b"sum1", true),
            (b"sum2", true),
            (b"sum3", true),   // empty sum tree - returns result
            (b"tree1", false), // empty tree with no root_key - returns 0 results
            (b"tree2", true),
        ];

        println!("\n=== Querying each element ===");

        for (key, expects_result) in &all_keys {
            let key_str = String::from_utf8_lossy(key);

            // Create a path query for this specific key
            let path_query = PathQuery::new(
                vec![pcount.to_vec()],
                SizedQuery::new(
                    Query::new_single_query_item(QueryItem::Key(key.to_vec())),
                    None,
                    None,
                ),
            );

            // Generate proof
            let proof = db
                .prove_query(&path_query, None, grove_version)
                .unwrap()
                .expect("should generate proof");

            // Verify proof and get parent tree info (which includes the count)
            let (verified_hash, parent_tree_type, results) =
                GroveDb::verify_query_get_parent_tree_info(&proof, &path_query, grove_version)
                    .expect("should verify proof");

            println!(
                "Key '{}': parent_tree_type={:?}, results={}",
                key_str,
                parent_tree_type,
                results.len()
            );

            // Verify the parent tree type is ProvableCountedMerkNode with count 19
            match parent_tree_type {
                TreeFeatureType::ProvableCountedMerkNode(count) => {
                    assert_eq!(
                        count, 19,
                        "Parent tree count for '{}' should be 19, got {}",
                        key_str, count
                    );
                }
                _ => panic!(
                    "Expected ProvableCountedMerkNode for '{}', got {:?}",
                    key_str, parent_tree_type
                ),
            }

            // Verify we got expected number of results
            if *expects_result {
                assert_eq!(
                    results.len(),
                    1,
                    "Should get exactly 1 result for key '{}'",
                    key_str
                );
            }

            // Verify the root hash matches
            let expected_root_hash = db.root_hash(None, grove_version).unwrap().unwrap();
            assert_eq!(
                verified_hash, expected_root_hash,
                "Root hash mismatch for key '{}'",
                key_str
            );
        }

        // =================================================================
        // STEP 9: Query all elements at once with RangeFull
        // =================================================================
        println!("\n=== Querying all elements with RangeFull ===");

        let range_query = PathQuery::new(
            vec![pcount.to_vec()],
            SizedQuery::new(
                Query::new_single_query_item(QueryItem::RangeFull(..)),
                None,
                None,
            ),
        );

        let proof = db
            .prove_query(&range_query, None, grove_version)
            .unwrap()
            .expect("should generate range proof");

        let (verified_hash, parent_tree_type, results) =
            GroveDb::verify_query_get_parent_tree_info(&proof, &range_query, grove_version)
                .expect("should verify range proof");

        println!("RangeFull query: {} results", results.len());
        println!("Parent tree type: {:?}", parent_tree_type);

        // Should get 14 elements (15 total minus tree1 which is empty and returns no result)
        // Total elements: cnt1, cnt2, cntsum1, item1-7, sum1, sum2, sum3, tree1, tree2 = 15
        // tree1 is an empty Tree with no root_key, so it doesn't appear in query results
        assert_eq!(results.len(), 14, "RangeFull should return 14 elements (15 - 1 empty tree)");

        // Verify parent tree type has count 19
        match parent_tree_type {
            TreeFeatureType::ProvableCountedMerkNode(count) => {
                assert_eq!(count, 19, "Parent tree count should be 19, got {}", count);
            }
            _ => panic!(
                "Expected ProvableCountedMerkNode, got {:?}",
                parent_tree_type
            ),
        }

        // Verify root hash
        let expected_root_hash = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(verified_hash, expected_root_hash, "Root hash mismatch");

        println!("\n=== All assertions passed! ===");
        println!("Tree structure verified:");
        println!("  - 2 normal trees (tree1, tree2) -> count 1 each = 2");
        println!("  - 3 sum trees (sum1, sum2, sum3) -> count 1 each = 3");
        println!("  - 2 count trees: cnt1 (3 items), cnt2 (1 item) -> count 3+1 = 4");
        println!("  - 1 count sum tree: cntsum1 (3 items) -> count 3");
        println!("  - 7 items (item1-item7) -> count 7");
        println!("  - Total provable count: 2+3+4+3+7 = 19");
    }

    #[test]
    fn test_provable_count_tree_nested_queries() {
        //! Test querying items inside nested subtrees
        //!
        //! Tree Structure:
        //! ```text
        //!                    [root]
        //!                       |
        //!                ProvableCountTree("pcount")  [count=4]
        //!                       |
        //!         +-------------+-------------+
        //!         |             |             |
        //!      Tree          SumTree      CountTree
        //!     "tree1"        "sum1"        "cnt1"
        //!     [cnt=1]        [cnt=1]       [cnt=2]
        //!         |             |             |
        //!     Item"a"      SumItem"b"     Item"c"
        //!                   =500          Item"d"
        //!
        //! Count = tree1(1) + sum1(1) + cnt1(2) = 4
        //! ```

        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        let pcount = b"pcount";
        let tree1 = b"tree1";
        let sum1 = b"sum1";
        let cnt1 = b"cnt1";

        // Create ProvableCountTree
        db.insert(
            &[] as &[&[u8]],
            pcount,
            Element::empty_provable_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert pcount");

        // Insert tree1 with item
        db.insert(
            &[pcount.as_slice()],
            tree1,
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert tree1");

        db.insert(
            &[pcount.as_slice(), tree1.as_slice()],
            b"a",
            Element::new_item(b"value_a".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert a");

        // Insert sum1 with sum item
        db.insert(
            &[pcount.as_slice()],
            sum1,
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert sum1");

        db.insert(
            &[pcount.as_slice(), sum1.as_slice()],
            b"b",
            Element::new_sum_item(500),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert b");

        // Insert cnt1 with items
        db.insert(
            &[pcount.as_slice()],
            cnt1,
            Element::empty_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert cnt1");

        db.insert(
            &[pcount.as_slice(), cnt1.as_slice()],
            b"c",
            Element::new_item(b"value_c".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert c");

        db.insert(
            &[pcount.as_slice(), cnt1.as_slice()],
            b"d",
            Element::new_item(b"value_d".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert d");

        // Verify pcount has count 4 (tree1=1, sum1=1, cnt1=2)
        let pcount_elem = db
            .get(&[] as &[&[u8]], pcount, None, grove_version)
            .unwrap()
            .expect("get pcount");

        match &pcount_elem {
            Element::ProvableCountTree(_, count, _) => {
                assert_eq!(*count, 4, "pcount should have count 4 (tree1=1 + sum1=1 + cnt1=2)");
            }
            _ => panic!("Expected ProvableCountTree"),
        }

        // Query item "a" inside tree1 (path: pcount/tree1, key: a)
        println!("\n=== Querying nested item 'a' in tree1 ===");
        let query_a = PathQuery::new(
            vec![pcount.to_vec(), tree1.to_vec()],
            SizedQuery::new(
                Query::new_single_query_item(QueryItem::Key(b"a".to_vec())),
                None,
                None,
            ),
        );

        let proof_a = db
            .prove_query(&query_a, None, grove_version)
            .unwrap()
            .expect("prove query a");

        let (hash_a, results_a) = GroveDb::verify_query(&proof_a, &query_a, grove_version)
            .expect("verify query a");

        assert_eq!(results_a.len(), 1, "Should find item a");
        println!("Found item 'a': {:?}", results_a[0]);

        // Verify root hash
        let expected_hash = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(hash_a, expected_hash, "Root hash should match for query a");

        // Query sum item "b" inside sum1
        println!("\n=== Querying nested sum item 'b' in sum1 ===");
        let query_b = PathQuery::new(
            vec![pcount.to_vec(), sum1.to_vec()],
            SizedQuery::new(
                Query::new_single_query_item(QueryItem::Key(b"b".to_vec())),
                None,
                None,
            ),
        );

        let proof_b = db
            .prove_query(&query_b, None, grove_version)
            .unwrap()
            .expect("prove query b");

        let (hash_b, results_b) = GroveDb::verify_query(&proof_b, &query_b, grove_version)
            .expect("verify query b");

        assert_eq!(results_b.len(), 1, "Should find sum item b");
        println!("Found sum item 'b': {:?}", results_b[0]);
        assert_eq!(hash_b, expected_hash, "Root hash should match for query b");

        // Query items in cnt1
        println!("\n=== Querying nested items in cnt1 ===");
        let query_cnt1 = PathQuery::new(
            vec![pcount.to_vec(), cnt1.to_vec()],
            SizedQuery::new(
                Query::new_single_query_item(QueryItem::RangeFull(..)),
                None,
                None,
            ),
        );

        let proof_cnt1 = db
            .prove_query(&query_cnt1, None, grove_version)
            .unwrap()
            .expect("prove query cnt1");

        let (hash_cnt1, results_cnt1) =
            GroveDb::verify_query(&proof_cnt1, &query_cnt1, grove_version)
                .expect("verify query cnt1");

        assert_eq!(results_cnt1.len(), 2, "Should find 2 items in cnt1");
        println!("Found {} items in cnt1", results_cnt1.len());
        assert_eq!(
            hash_cnt1, expected_hash,
            "Root hash should match for query cnt1"
        );

        println!("\n=== Nested query test passed! ===");
    }
}
