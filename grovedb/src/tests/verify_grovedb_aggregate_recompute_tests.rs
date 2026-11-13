//! `verify_grovedb` recomputes each tree's aggregate from the elements it
//! holds.
//!
//! A Merk stores its aggregate twice over: every node carries its own
//! contribution, and every link carries the total of the subtree below it.
//! The parent's tree element then records the root's total. Comparing the
//! parent's recorded value with the root's total compares two stored values,
//! so these tests plant a node whose stored contribution disagrees with the
//! element it holds, let propagation carry the drifted total to every
//! ancestor, and assert the tree is reported.

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use grovedb_merk::{
        element::insert::ElementInsertToStorageExtensions, tree::TreeFeatureType, CryptoHash,
    };
    use grovedb_path::SubtreePath;
    use grovedb_storage::{Storage, StorageBatch};
    use grovedb_version::version::GroveVersion;

    use crate::{
        batch::QualifiedGroveDbOp,
        elements_aggregate_labels, merge_child_issues,
        reference_path::ReferencePathType,
        tests::{make_test_grovedb, TEST_LEAF},
        AggregateData, Element, ElementsAggregate, GroveDb, VerificationIssues,
    };

    // -----------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------

    fn verify(db: &GroveDb, grove_version: &GroveVersion) -> VerificationIssues {
        db.verify_grovedb(None, true, true, grove_version)
            .expect("verify_grovedb must not return a hard error")
    }

    fn insert(db: &GroveDb, path: &[&[u8]], key: &[u8], element: Element, gv: &GroveVersion) {
        db.insert(path, key, element, None, None, gv)
            .unwrap()
            .expect("insert");
    }

    fn get(db: &GroveDb, path: &[&[u8]], key: &[u8], gv: &GroveVersion) -> Element {
        db.get(path, key, None, gv).unwrap().expect("get")
    }

    fn placeholder(label: &str) -> CryptoHash {
        blake3::hash(label.as_bytes()).into()
    }

    fn owned(path: &[&[u8]]) -> Vec<Vec<u8>> {
        path.iter().map(|segment| segment.to_vec()).collect()
    }

    /// Write `element` at `key` of the tree at `path` with `feature_type` as
    /// its node's own aggregate, whatever the element is worth, then
    /// propagate. Propagation re-derives every hash and every ancestor's
    /// recorded aggregate from the Merk, so the database stays consistent in
    /// everything `verify_grovedb` compared before: only the node's stored
    /// aggregate and its element disagree.
    fn plant_node_aggregate(
        db: &GroveDb,
        path: &[&[u8]],
        key: &[u8],
        element: &Element,
        feature_type: TreeFeatureType,
        gv: &GroveVersion,
    ) {
        let tx = db.start_transaction();
        let batch = StorageBatch::new();
        let path: SubtreePath<&[u8]> = path.into();
        let mut merk = db
            .open_transactional_merk_at_path(path.clone(), &tx, Some(&batch), gv)
            .unwrap()
            .expect("open the tree's Merk");
        let mut operations = Vec::new();
        element
            .insert_into_batch_operations(key, &mut operations, feature_type, gv)
            .unwrap()
            .expect("build the put");
        merk.apply::<_, Vec<u8>>(&operations, &[], None, gv)
            .unwrap()
            .expect("apply the put");

        let mut merk_cache = HashMap::new();
        merk_cache.insert(path.clone(), merk);
        db.propagate_changes_with_transaction(merk_cache, path, &tx, &batch, gv)
            .unwrap()
            .expect("propagate");
        db.db
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .expect("commit");
        tx.commit().expect("tx commit");
    }

    /// Assert the only issue is the recomputed-aggregate one at `path`, with
    /// the given labels.
    fn assert_only_aggregate_issue(
        issues: &VerificationIssues,
        path: &[&[u8]],
        contributed_label: &str,
        stored_label: &str,
    ) {
        assert_eq!(
            issues.keys().cloned().collect::<Vec<_>>(),
            vec![owned(path)],
            "expected exactly the drifted tree to be reported: {issues:?}"
        );
        let (_, expected, actual) = issues[&owned(path)];
        assert_eq!(
            expected,
            placeholder(contributed_label),
            "the expected slot carries what the elements contribute"
        );
        assert_eq!(
            actual,
            placeholder(stored_label),
            "the actual slot carries the stored aggregate"
        );
    }

    // -----------------------------------------------------------------
    // Drift is reported
    // -----------------------------------------------------------------

    #[test]
    fn should_report_a_sum_tree_whose_stored_sum_differs_from_its_sum_items() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        insert(&db, &[TEST_LEAF], b"sum", Element::empty_sum_tree(), gv);
        insert(
            &db,
            &[TEST_LEAF, b"sum"],
            b"a",
            Element::new_sum_item(5),
            gv,
        );
        insert(
            &db,
            &[TEST_LEAF, b"sum"],
            b"b",
            Element::new_sum_item(7),
            gv,
        );
        assert!(verify(&db, gv).is_empty());

        // The node at `a` still holds SumItem(5) but now says it is worth
        // 1,000,000.
        plant_node_aggregate(
            &db,
            &[TEST_LEAF, b"sum"],
            b"a",
            &Element::new_sum_item(5),
            TreeFeatureType::SummedMerkNode(1_000_000),
            gv,
        );

        // The elements are untouched and the parent records the drifted
        // total, so recorded and stored agree with each other.
        assert_eq!(
            get(&db, &[TEST_LEAF, b"sum"], b"a", gv),
            Element::new_sum_item(5)
        );
        assert_eq!(
            get(&db, &[TEST_LEAF], b"sum", gv).sum_value_or_default(),
            1_000_007
        );

        assert_only_aggregate_issue(
            &verify(&db, gv),
            &[TEST_LEAF, b"sum"],
            "elements contribute count 0 sum 12",
            "stored aggregate Sum(1000007)",
        );
    }

    #[test]
    fn should_report_a_count_tree_whose_stored_count_differs_from_its_elements() {
        let gv = GroveVersion::latest();
        // The drifted node is the root of the Merk for one of these keys and
        // below the root for the others, so both a node's own value and a
        // link's recorded total are covered.
        for drifted_key in [b"a", b"b", b"c"] {
            let db = make_test_grovedb(gv);
            insert(&db, &[TEST_LEAF], b"count", Element::empty_count_tree(), gv);
            for key in [b"a", b"b", b"c"] {
                insert(
                    &db,
                    &[TEST_LEAF, b"count"],
                    key,
                    Element::new_item(b"v".to_vec()),
                    gv,
                );
            }
            assert!(verify(&db, gv).is_empty());

            plant_node_aggregate(
                &db,
                &[TEST_LEAF, b"count"],
                drifted_key,
                &Element::new_item(b"v".to_vec()),
                TreeFeatureType::CountedMerkNode(5),
                gv,
            );
            assert_eq!(
                get(&db, &[TEST_LEAF], b"count", gv).count_value_or_default(),
                7
            );

            assert_only_aggregate_issue(
                &verify(&db, gv),
                &[TEST_LEAF, b"count"],
                "elements contribute count 3 sum 0",
                "stored aggregate Count(7)",
            );
        }
    }

    #[test]
    fn should_report_drift_in_every_other_aggregate_tree_type() {
        let gv = GroveVersion::latest();
        // (tree, drifted feature type of the node at `a`, stored aggregate
        // afterwards). Every tree holds SumItem(5) at `a` and SumItem(7) at
        // `b`, so its elements contribute count 2 where it counts and sum 12
        // where it sums.
        let cases: Vec<(Element, TreeFeatureType, &str, &str)> = vec![
            (
                Element::empty_big_sum_tree(),
                TreeFeatureType::BigSummedMerkNode(1_000_000),
                "elements contribute count 0 sum 12",
                "stored aggregate BigSum(1000007)",
            ),
            (
                Element::empty_count_sum_tree(),
                TreeFeatureType::CountedSummedMerkNode(1, 1_000_000),
                "elements contribute count 2 sum 12",
                "stored aggregate CountAndSum(2, 1000007)",
            ),
            (
                Element::empty_count_sum_tree(),
                TreeFeatureType::CountedSummedMerkNode(4, 5),
                "elements contribute count 2 sum 12",
                "stored aggregate CountAndSum(5, 12)",
            ),
            (
                Element::empty_provable_count_sum_tree(),
                TreeFeatureType::ProvableCountedSummedMerkNode(4, 5),
                "elements contribute count 2 sum 12",
                "stored aggregate ProvableCountAndSum(5, 12)",
            ),
            (
                Element::empty_provable_sum_tree(),
                TreeFeatureType::ProvableSummedMerkNode(1_000_000),
                "elements contribute count 0 sum 12",
                "stored aggregate ProvableSum(1000007)",
            ),
            (
                Element::empty_provable_count_provable_sum_tree(),
                TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(1, -3),
                "elements contribute count 2 sum 12",
                "stored aggregate ProvableCountAndProvableSum(2, 4)",
            ),
        ];
        for (tree, drifted, contributed_label, stored_label) in cases {
            let db = make_test_grovedb(gv);
            insert(&db, &[TEST_LEAF], b"tree", tree, gv);
            insert(
                &db,
                &[TEST_LEAF, b"tree"],
                b"a",
                Element::new_sum_item(5),
                gv,
            );
            insert(
                &db,
                &[TEST_LEAF, b"tree"],
                b"b",
                Element::new_sum_item(7),
                gv,
            );
            assert!(verify(&db, gv).is_empty());

            plant_node_aggregate(
                &db,
                &[TEST_LEAF, b"tree"],
                b"a",
                &Element::new_sum_item(5),
                drifted,
                gv,
            );

            assert_only_aggregate_issue(
                &verify(&db, gv),
                &[TEST_LEAF, b"tree"],
                contributed_label,
                stored_label,
            );
        }
    }

    #[test]
    fn should_report_a_provable_count_tree_whose_stored_count_differs_from_its_elements() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        insert(
            &db,
            &[TEST_LEAF],
            b"pc",
            Element::empty_provable_count_tree(),
            gv,
        );
        for key in [b"a", b"b"] {
            insert(
                &db,
                &[TEST_LEAF, b"pc"],
                key,
                Element::new_item(b"v".to_vec()),
                gv,
            );
        }
        assert!(verify(&db, gv).is_empty());

        // The count is part of every node hash in this tree type. Propagation
        // hashes the drifted count in, so the hash chain still agrees.
        plant_node_aggregate(
            &db,
            &[TEST_LEAF, b"pc"],
            b"a",
            &Element::new_item(b"v".to_vec()),
            TreeFeatureType::ProvableCountedMerkNode(9),
            gv,
        );

        assert_only_aggregate_issue(
            &verify(&db, gv),
            &[TEST_LEAF, b"pc"],
            "elements contribute count 2 sum 0",
            "stored aggregate ProvableCount(10)",
        );
    }

    #[test]
    fn should_report_only_the_drifted_tree_when_it_is_nested_in_aggregate_trees() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        insert(&db, &[TEST_LEAF], b"outer", Element::empty_sum_tree(), gv);
        insert(
            &db,
            &[TEST_LEAF, b"outer"],
            b"own",
            Element::new_sum_item(100),
            gv,
        );
        insert(
            &db,
            &[TEST_LEAF, b"outer"],
            b"inner",
            Element::empty_sum_tree(),
            gv,
        );
        insert(
            &db,
            &[TEST_LEAF, b"outer", b"inner"],
            b"a",
            Element::new_sum_item(5),
            gv,
        );
        assert!(verify(&db, gv).is_empty());

        plant_node_aggregate(
            &db,
            &[TEST_LEAF, b"outer", b"inner"],
            b"a",
            &Element::new_sum_item(5),
            TreeFeatureType::SummedMerkNode(50),
            gv,
        );

        // A child tree contributes the aggregate its element records. The
        // inner element records 50, so the outer tree's 150 is what its
        // elements contribute and only the inner tree is reported.
        assert_eq!(
            get(&db, &[TEST_LEAF], b"outer", gv).sum_value_or_default(),
            150
        );
        assert_only_aggregate_issue(
            &verify(&db, gv),
            &[TEST_LEAF, b"outer", b"inner"],
            "elements contribute count 0 sum 5",
            "stored aggregate Sum(50)",
        );
    }

    #[test]
    fn should_report_a_drifted_count_tree_under_an_indexed_tree() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        insert(
            &db,
            &[TEST_LEAF],
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            gv,
        );
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"ct",
            Element::empty_count_tree(),
            None,
            gv,
        )
        .unwrap()
        .expect("count tree entry");
        for key in [b"x", b"y", b"z"] {
            insert(
                &db,
                &[TEST_LEAF, b"cidx", b"ct"],
                key,
                Element::new_item(b"v".to_vec()),
                gv,
            );
        }
        assert!(verify(&db, gv).is_empty());

        plant_node_aggregate(
            &db,
            &[TEST_LEAF, b"cidx", b"ct"],
            b"x",
            &Element::new_item(b"v".to_vec()),
            TreeFeatureType::CountedMerkNode(4),
            gv,
        );

        // The walk reaches the count tree through the indexed tree's primary
        // and reports it; the primary and its secondary row follow the
        // entry's recorded count and stay clean.
        assert_only_aggregate_issue(
            &verify(&db, gv),
            &[TEST_LEAF, b"cidx", b"ct"],
            "elements contribute count 3 sum 0",
            "stored aggregate Count(6)",
        );
    }

    #[test]
    fn should_report_sum_items_totalling_more_than_the_tree_can_hold() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        insert(&db, &[TEST_LEAF], b"sum", Element::empty_sum_tree(), gv);
        insert(
            &db,
            &[TEST_LEAF, b"sum"],
            b"a",
            Element::new_sum_item(1),
            gv,
        );
        insert(
            &db,
            &[TEST_LEAF, b"sum"],
            b"b",
            Element::new_sum_item(1),
            gv,
        );

        // Two sum items of i64::MAX cannot be written: their total does not
        // fit the tree's i64. Planted with a node value of 1 each, the stored
        // total is 2 while the elements add up past i64.
        for key in [b"a", b"b"] {
            plant_node_aggregate(
                &db,
                &[TEST_LEAF, b"sum"],
                key,
                &Element::new_sum_item(i64::MAX),
                TreeFeatureType::SummedMerkNode(1),
                gv,
            );
        }

        assert_only_aggregate_issue(
            &verify(&db, gv),
            &[TEST_LEAF, b"sum"],
            &format!("elements contribute count 0 sum {}", 2 * (i64::MAX as i128)),
            "stored aggregate Sum(2)",
        );
    }

    // -----------------------------------------------------------------
    // A clean database stays clean
    // -----------------------------------------------------------------

    #[test]
    fn should_report_no_issues_for_nested_aggregate_trees_with_wrapped_elements() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);

        // --- CountSumTree: every wrapper is accepted here.
        let cs: &[&[u8]] = &[TEST_LEAF, b"cs"];
        insert(
            &db,
            &[TEST_LEAF],
            b"cs",
            Element::empty_count_sum_tree(),
            gv,
        );
        insert(&db, cs, b"item", Element::new_item(b"v".to_vec()), gv);
        insert(&db, cs, b"sum_item", Element::new_sum_item(3), gv);
        insert(
            &db,
            cs,
            b"item_with_sum",
            Element::new_item_with_sum_item(b"v".to_vec(), 4),
            gv,
        );
        insert(
            &db,
            cs,
            b"non_counted_sum_item",
            Element::new_non_counted(Element::new_sum_item(10)).expect("wrap"),
            gv,
        );
        insert(
            &db,
            cs,
            b"ref_with_sum",
            Element::new_reference_with_sum_item(
                ReferencePathType::AbsolutePathReference(vec![
                    TEST_LEAF.to_vec(),
                    b"cs".to_vec(),
                    b"item".to_vec(),
                ]),
                -2,
            ),
            gv,
        );
        insert(
            &db,
            cs,
            b"not_summed",
            Element::new_not_summed(Element::empty_sum_tree()).expect("wrap"),
            gv,
        );
        insert(
            &db,
            &[TEST_LEAF, b"cs", b"not_summed"],
            b"a",
            Element::new_sum_item(1_000),
            gv,
        );
        insert(
            &db,
            cs,
            b"not_counted_or_summed",
            Element::new_not_counted_or_summed(Element::empty_count_sum_tree()).expect("wrap"),
            gv,
        );
        for (key, value) in [(b"a", 20_000), (b"b", 30_000)] {
            insert(
                &db,
                &[TEST_LEAF, b"cs", b"not_counted_or_summed"],
                key,
                Element::new_sum_item(value),
                gv,
            );
        }
        insert(
            &db,
            cs,
            b"non_counted_tree",
            Element::new_non_counted(Element::empty_count_sum_tree()).expect("wrap"),
            gv,
        );
        insert(
            &db,
            &[TEST_LEAF, b"cs", b"non_counted_tree"],
            b"a",
            Element::new_sum_item(6),
            gv,
        );
        insert(&db, cs, b"nested", Element::empty_count_sum_tree(), gv);
        for (key, value) in [(b"a", 7), (b"b", -8), (b"c", 9)] {
            insert(
                &db,
                &[TEST_LEAF, b"cs", b"nested"],
                key,
                Element::new_sum_item(value),
                gv,
            );
        }
        // count: item 1 + sum_item 1 + item_with_sum 1 + ref 1 + not_summed 1
        //        + nested 3 = 8 (the wrapped-out entries add 0)
        // sum:   3 + 4 + 10 - 2 + 6 (non_counted_tree) + 8 (nested) = 29
        assert_eq!(
            get(&db, &[TEST_LEAF], b"cs", gv).count_sum_value_or_default(),
            (8, 29)
        );

        // --- SumTree with nested sum, big sum and count trees, written
        // through a batch.
        let sum_path = vec![TEST_LEAF.to_vec(), b"s".to_vec()];
        let nested_path = vec![TEST_LEAF.to_vec(), b"s".to_vec(), b"nested".to_vec()];
        db.apply_batch(
            vec![
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"s".to_vec(),
                    Element::empty_sum_tree(),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    sum_path.clone(),
                    b"a".to_vec(),
                    Element::new_sum_item(-40),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    sum_path.clone(),
                    b"b".to_vec(),
                    Element::new_sum_item(15),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    sum_path.clone(),
                    b"nested".to_vec(),
                    Element::empty_sum_tree(),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    nested_path.clone(),
                    b"a".to_vec(),
                    Element::new_sum_item(11),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    nested_path,
                    b"b".to_vec(),
                    Element::new_item_with_sum_item(b"v".to_vec(), 12),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    sum_path.clone(),
                    b"not_summed_big".to_vec(),
                    Element::new_not_summed(Element::empty_big_sum_tree()).expect("wrap"),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    sum_path.clone(),
                    b"counted".to_vec(),
                    Element::empty_count_tree(),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"s".to_vec(), b"counted".to_vec()],
                    b"a".to_vec(),
                    Element::new_item(b"v".to_vec()),
                ),
            ],
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("batch");
        insert(
            &db,
            &[TEST_LEAF, b"s", b"not_summed_big"],
            b"a",
            Element::new_sum_item(i64::MAX),
            gv,
        );
        assert_eq!(get(&db, &[TEST_LEAF], b"s", gv).sum_value_or_default(), -2);

        // --- CountTree with a non-counted item and nested trees.
        let count: &[&[u8]] = &[TEST_LEAF, b"c"];
        insert(&db, &[TEST_LEAF], b"c", Element::empty_count_tree(), gv);
        insert(&db, count, b"a", Element::new_item(b"v".to_vec()), gv);
        insert(
            &db,
            count,
            b"non_counted",
            Element::new_non_counted(Element::new_item(b"v".to_vec())).expect("wrap"),
            gv,
        );
        insert(&db, count, b"nested", Element::empty_count_tree(), gv);
        for key in [b"a", b"b"] {
            insert(
                &db,
                &[TEST_LEAF, b"c", b"nested"],
                key,
                Element::new_item(b"v".to_vec()),
                gv,
            );
        }
        insert(&db, count, b"plain", Element::empty_tree(), gv);
        insert(
            &db,
            &[TEST_LEAF, b"c", b"plain"],
            b"a",
            Element::new_item(b"v".to_vec()),
            gv,
        );
        // a 1 + nested 2 + plain 1 (a tree without a count counts as one).
        assert_eq!(get(&db, &[TEST_LEAF], b"c", gv).count_value_or_default(), 4);

        // --- BigSumTree holding sum items and sum trees of both widths.
        let big: &[&[u8]] = &[TEST_LEAF, b"big"];
        insert(&db, &[TEST_LEAF], b"big", Element::empty_big_sum_tree(), gv);
        insert(&db, big, b"a", Element::new_sum_item(i64::MAX), gv);
        insert(&db, big, b"b", Element::new_sum_item(i64::MAX), gv);
        insert(&db, big, b"nested", Element::empty_sum_tree(), gv);
        insert(
            &db,
            &[TEST_LEAF, b"big", b"nested"],
            b"a",
            Element::new_sum_item(-1),
            gv,
        );
        insert(&db, big, b"nested_big", Element::empty_big_sum_tree(), gv);
        insert(
            &db,
            &[TEST_LEAF, b"big", b"nested_big"],
            b"a",
            Element::new_sum_item(i64::MAX),
            gv,
        );
        assert_eq!(
            get(&db, &[TEST_LEAF], b"big", gv).big_sum_value_or_default(),
            3 * (i64::MAX as i128) - 1
        );

        // --- Provable trees, nested.
        insert(
            &db,
            &[TEST_LEAF],
            b"pcps",
            Element::empty_provable_count_provable_sum_tree(),
            gv,
        );
        insert(
            &db,
            &[TEST_LEAF, b"pcps"],
            b"a",
            Element::new_sum_item(21),
            gv,
        );
        insert(
            &db,
            &[TEST_LEAF, b"pcps"],
            b"nested",
            Element::empty_provable_count_tree(),
            gv,
        );
        for key in [b"a", b"b", b"c"] {
            insert(
                &db,
                &[TEST_LEAF, b"pcps", b"nested"],
                key,
                Element::new_item(b"v".to_vec()),
                gv,
            );
        }
        insert(
            &db,
            &[TEST_LEAF, b"pcps"],
            b"nested_sum",
            Element::empty_provable_sum_tree(),
            gv,
        );
        insert(
            &db,
            &[TEST_LEAF, b"pcps", b"nested_sum"],
            b"a",
            Element::new_sum_item(-4),
            gv,
        );
        assert_eq!(
            get(&db, &[TEST_LEAF], b"pcps", gv).count_sum_value_or_default(),
            (5, 17)
        );

        // --- An indexed tree with populated entries.
        insert(
            &db,
            &[TEST_LEAF],
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            gv,
        );
        for (entry, keys) in [(b"one", vec![b"a"]), (b"two", vec![b"a", b"b"])] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                entry,
                Element::empty_count_tree(),
                None,
                gv,
            )
            .unwrap()
            .expect("count tree entry");
            for key in keys {
                insert(
                    &db,
                    &[TEST_LEAF, b"cidx", entry],
                    key,
                    Element::new_item(b"v".to_vec()),
                    gv,
                );
            }
        }

        // Deletes and replacements go through the aggregate updates too.
        db.delete(cs, b"sum_item", None, None, gv)
            .unwrap()
            .expect("delete");
        insert(
            &db,
            &[TEST_LEAF, b"cs", b"nested"],
            b"b",
            Element::new_sum_item(80),
            gv,
        );
        assert_eq!(
            get(&db, &[TEST_LEAF], b"cs", gv).count_sum_value_or_default(),
            (7, 114)
        );

        let issues = verify(&db, gv);
        assert!(issues.is_empty(), "unexpected issues: {issues:?}");
    }

    // -----------------------------------------------------------------
    // The comparison and the merge
    // -----------------------------------------------------------------

    #[test]
    fn should_treat_an_empty_merk_as_contributing_and_storing_nothing() {
        assert_eq!(
            elements_aggregate_labels(
                &ElementsAggregate::default(),
                &Ok(AggregateData::NoAggregateData)
            ),
            None
        );
    }

    #[test]
    fn should_compare_the_count_and_the_sum_separately() {
        let mut contributed = ElementsAggregate::default();
        contributed.add(TreeFeatureType::CountedSummedMerkNode(1, 5));
        contributed.add(TreeFeatureType::CountedSummedMerkNode(1, 7));

        assert_eq!(
            elements_aggregate_labels(&contributed, &Ok(AggregateData::CountAndSum(2, 12))),
            None
        );
        for stored in [
            AggregateData::CountAndSum(3, 12),
            AggregateData::CountAndSum(2, 13),
            AggregateData::NoAggregateData,
        ] {
            assert_eq!(
                elements_aggregate_labels(&contributed, &Ok(stored)),
                Some((
                    "elements contribute count 2 sum 12".to_string(),
                    format!("stored aggregate {stored:?}"),
                ))
            );
        }
    }

    #[test]
    fn should_report_an_overflowing_count_or_sum_instead_of_panicking() {
        let mut count = ElementsAggregate::default();
        count.add(TreeFeatureType::CountedMerkNode(u64::MAX));
        count.add(TreeFeatureType::CountedMerkNode(1));
        assert_eq!(
            elements_aggregate_labels(&count, &Ok(AggregateData::Count(0))),
            Some((
                "elements contribute an overflowing count, sum 0".to_string(),
                "stored aggregate Count(0)".to_string(),
            ))
        );

        let mut sum = ElementsAggregate::default();
        sum.add(TreeFeatureType::BigSummedMerkNode(i128::MAX));
        sum.add(TreeFeatureType::BigSummedMerkNode(1));
        // An overflowed total stays overflowed whatever is added afterwards.
        sum.add(TreeFeatureType::BigSummedMerkNode(-1));
        assert_eq!(
            elements_aggregate_labels(&sum, &Ok(AggregateData::BigSum(i128::MAX))),
            Some((
                "elements contribute count 0, an overflowing sum".to_string(),
                format!("stored aggregate BigSum({})", i128::MAX),
            ))
        );

        let mut both = ElementsAggregate::default();
        both.add(TreeFeatureType::CountedSummedMerkNode(u64::MAX, i64::MIN));
        both.add(TreeFeatureType::CountedMerkNode(1));
        both.add(TreeFeatureType::BigSummedMerkNode(i128::MIN));
        assert_eq!(
            elements_aggregate_labels(&both, &Ok(AggregateData::NoAggregateData)),
            Some((
                "elements contribute an overflowing count and sum".to_string(),
                "stored aggregate NoAggregateData".to_string(),
            ))
        );
    }

    #[test]
    fn should_report_a_stored_aggregate_that_cannot_be_read() {
        let mut contributed = ElementsAggregate::default();
        contributed.add(TreeFeatureType::SummedMerkNode(5));
        assert_eq!(
            elements_aggregate_labels(&contributed, &Err("sum is overflowing".to_string())),
            Some((
                "elements contribute count 0 sum 5".to_string(),
                "stored aggregate cannot be read: sum is overflowing".to_string(),
            ))
        );
    }

    #[test]
    fn should_keep_an_issue_already_recorded_for_a_tree_when_merging_its_own_issues() {
        let tree = owned(&[TEST_LEAF, b"sum"]);
        let element = owned(&[TEST_LEAF, b"sum", b"a"]);
        let hash_mismatch = ([1u8; 32], [2u8; 32], [3u8; 32]);
        let aggregate_mismatch = ([4u8; 32], [5u8; 32], [6u8; 32]);
        let element_issue = ([7u8; 32], [8u8; 32], [9u8; 32]);

        let mut issues: VerificationIssues = HashMap::new();
        issues.insert(tree.clone(), hash_mismatch);
        let mut child_issues: VerificationIssues = HashMap::new();
        child_issues.insert(tree.clone(), aggregate_mismatch);
        child_issues.insert(element.clone(), element_issue);

        merge_child_issues(&mut issues, child_issues);

        assert_eq!(issues.len(), 2);
        assert_eq!(issues[&tree], hash_mismatch);
        assert_eq!(issues[&element], element_issue);
    }
}
