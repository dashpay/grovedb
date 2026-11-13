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

    use grovedb_element::indexed::IndexAxis;
    use grovedb_merk::{
        element::{
            get::ElementFetchFromStorageExtensions, insert::ElementInsertToStorageExtensions,
            reconstruct::ElementReconstructExtensions,
        },
        tree::{kv::ValueDefinedCostType, TreeFeatureType},
        tree_type::TreeType,
        CryptoHash,
    };
    use grovedb_path::SubtreePath;
    use grovedb_storage::{Storage, StorageBatch};
    use grovedb_version::version::GroveVersion;

    use crate::{
        batch::QualifiedGroveDbOp,
        elements_aggregate_labels, merge_child_issues,
        operations::indexed_tree::make_axis_secondary_key,
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

    /// Write `element` at `key` of the tree at `path` with `feature_type` as
    /// its node's own aggregate and commit, without propagating. For a
    /// planted value that makes the tree's stored aggregate unreadable:
    /// propagation reads that aggregate and fails.
    fn plant_node_aggregate_without_propagating(
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
            .open_transactional_merk_at_path(path, &tx, Some(&batch), gv)
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
        drop(merk);
        db.db
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .expect("commit");
        tx.commit().expect("tx commit");
    }

    /// The root key the indexed tree element records for its `axis`
    /// secondary.
    fn recorded_secondary_root_key(indexed_tree: &Element, axis: IndexAxis) -> Option<Vec<u8>> {
        match indexed_tree.underlying() {
            Element::ProvableCountIndexedTree(_, secondary_root_key, ..)
            | Element::ProvableSumIndexedTree(_, secondary_root_key, ..) => {
                secondary_root_key.clone()
            }
            Element::ProvableCountProvableSumIndexedTree(_, _, _, axes, _) => axes
                .iter()
                .find(|(tag, _)| IndexAxis::try_from_tag(*tag).expect("axis tag") == axis)
                .and_then(|(_, secondary_root_key)| secondary_root_key.clone()),
            other => panic!("not an indexed tree: {other:?}"),
        }
    }

    /// In the `axis` secondary of the indexed tree at `tree_path`, write the
    /// row at `row_key` (the row of the entry `item_key`) back unchanged with
    /// `feature_type` as its node's own aggregate, and commit. Every row
    /// stays the one the primary implies, bound to its entry as before; only
    /// the node's stored aggregate and its row disagree. Nothing is
    /// propagated: rewriting an existing key leaves the secondary's root key
    /// where the indexed tree element records it.
    fn plant_secondary_row_aggregate(
        db: &GroveDb,
        tree_path: &[&[u8]],
        axis: IndexAxis,
        item_key: &[u8],
        row_key: &[u8],
        feature_type: TreeFeatureType,
        gv: &GroveVersion,
    ) {
        let (tree_key, parent_path) = tree_path.split_last().expect("non-root indexed tree");
        let indexed_tree = get(db, parent_path, tree_key, gv);

        let tx = db.start_transaction();
        let batch = StorageBatch::new();
        let path: SubtreePath<&[u8]> = tree_path.into();
        // A row is a reference committed together with the value hash of the
        // primary entry it points at.
        let entry_value_hash = db
            .open_transactional_merk_at_path(path.clone(), &tx, Some(&batch), gv)
            .unwrap()
            .expect("open the primary")
            .get_value_hash(
                item_key,
                true,
                None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                gv,
            )
            .unwrap()
            .expect("read the entry's value hash")
            .expect("the entry is there");
        let mut secondary = db
            .open_indexed_secondary_at_path(
                path,
                axis,
                recorded_secondary_root_key(&indexed_tree, axis),
                &tx,
                Some(&batch),
                gv,
            )
            .unwrap()
            .expect("open the secondary");
        let row = Element::get(&secondary, row_key, true, gv)
            .unwrap()
            .expect("the row is there");
        let mut operations = Vec::new();
        row.insert_reference_into_batch_operations(
            row_key,
            entry_value_hash,
            &mut operations,
            feature_type,
            gv,
        )
        .unwrap()
        .expect("build the put");
        secondary
            .apply::<_, Vec<u8>>(&operations, &[], None, gv)
            .unwrap()
            .expect("apply the put");
        drop(secondary);
        db.db
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .expect("commit");
        tx.commit().expect("tx commit");
    }

    /// Rebind the count indexed tree element at `tree_path` to the root
    /// hashes its primary and its secondary have now, and propagate, so the
    /// hash chain is whole again after a write to the secondary.
    fn rebind_count_indexed_tree(db: &GroveDb, tree_path: &[&[u8]], gv: &GroveVersion) {
        let tx = db.start_transaction();
        let batch = StorageBatch::new();
        let path: SubtreePath<&[u8]> = tree_path.into();
        let (parent_path, tree_key) = path.derive_parent().expect("non-root indexed tree");

        let (primary_root_hash, primary_root_key, primary_aggregate) = db
            .open_transactional_merk_at_path(path.clone(), &tx, Some(&batch), gv)
            .unwrap()
            .expect("open the primary")
            .root_hash_key_and_aggregate_data()
            .unwrap()
            .expect("primary root state");

        let mut parent_merk = db
            .open_transactional_merk_at_path(parent_path.clone(), &tx, Some(&batch), gv)
            .unwrap()
            .expect("open the parent");
        let indexed_tree = Element::get(&parent_merk, tree_key, true, gv)
            .unwrap()
            .expect("indexed tree element");
        let (secondary_root_hash, secondary_root_key, _) = db
            .open_indexed_secondary_at_path(
                path,
                IndexAxis::Count,
                recorded_secondary_root_key(&indexed_tree, IndexAxis::Count),
                &tx,
                Some(&batch),
                gv,
            )
            .unwrap()
            .expect("open the secondary")
            .root_hash_key_and_aggregate_data()
            .unwrap()
            .expect("secondary root state");

        indexed_tree
            .reconstruct_with_two_root_keys(primary_root_key, secondary_root_key, primary_aggregate)
            .expect("reconstruct the indexed tree element")
            .insert_count_indexed_subtree(
                &mut parent_merk,
                tree_key,
                primary_root_hash,
                secondary_root_hash,
                None,
                gv,
            )
            .unwrap()
            .expect("rebind the indexed tree element");

        let mut merk_cache = HashMap::new();
        merk_cache.insert(parent_path.clone(), parent_merk);
        db.propagate_changes_with_transaction(merk_cache, parent_path, &tx, &batch, gv)
            .unwrap()
            .expect("propagate");
        db.db
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .expect("commit");
        tx.commit().expect("tx commit");
    }

    /// The reason the Merk of the tree at `path` gives for not being able to
    /// read its stored aggregate.
    fn unreadable_aggregate_reason(db: &GroveDb, path: &[&[u8]], gv: &GroveVersion) -> String {
        let tx = db.start_transaction();
        let path: SubtreePath<&[u8]> = path.into();
        db.open_transactional_merk_at_path(path, &tx, None, gv)
            .unwrap()
            .expect("open the tree's Merk")
            .aggregate_data()
            .expect_err("the stored aggregate cannot be read")
            .to_string()
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
    // Indexed tree secondaries
    // -----------------------------------------------------------------

    /// A count indexed tree at `[TEST_LEAF, cidx]` holding the items `a`,
    /// `b` and `c`.
    fn make_count_indexed_tree(db: &GroveDb, gv: &GroveVersion) {
        insert(
            db,
            &[TEST_LEAF],
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            gv,
        );
        for key in [b"a", b"b", b"c"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                key,
                Element::new_item(key.to_vec()),
                None,
                gv,
            )
            .unwrap()
            .expect("count indexed tree entry");
        }
    }

    #[test]
    fn should_report_an_indexed_tree_secondary_whose_stored_aggregate_differs_from_its_rows() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        make_count_indexed_tree(&db, gv);
        assert!(verify(&db, gv).is_empty());

        // Each item counts 1, so its row sits under count 1 and is worth one
        // row carrying 1. The node of `a`'s row is planted as five rows; the
        // row itself is written back as it was, and the indexed tree element
        // is rebound so the hash chain holds.
        plant_secondary_row_aggregate(
            &db,
            &[TEST_LEAF, b"cidx"],
            IndexAxis::Count,
            b"a",
            &make_axis_secondary_key(IndexAxis::Count, 1, 0, b"a"),
            TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(5, 1),
            gv,
        );
        rebind_count_indexed_tree(&db, &[TEST_LEAF, b"cidx"], gv);

        // Every row is still the one the primary implies and every hash
        // matches: only the recomputed total tells.
        assert_only_aggregate_issue(
            &verify(&db, gv),
            &[TEST_LEAF, b"cidx", b"__cidx_secondary_aggregate__"],
            "elements contribute count 3 sum 3",
            "stored aggregate ProvableCountAndProvableSum(7, 3)",
        );
    }

    #[test]
    fn should_report_drift_in_the_secondary_of_every_indexed_tree_axis() {
        let gv = GroveVersion::latest();

        // (axis, sentinel, node planted for `a`'s row, stored aggregate).
        // The entries are worth sums 10 and 20, and count 1 each.
        let sum_drift = (
            TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(1, 11),
            "stored aggregate ProvableCountAndProvableSum(2, 31)",
        );

        // Sum indexed tree: one sum axis.
        {
            let db = make_test_grovedb(gv);
            insert(
                &db,
                &[TEST_LEAF],
                b"psit",
                Element::empty_provable_sum_indexed_tree(),
                gv,
            );
            for (key, sum) in [(b"a", 10), (b"b", 20)] {
                db.insert_into_provable_sum_indexed_tree(
                    [TEST_LEAF, b"psit"].as_ref(),
                    key,
                    Element::new_sum_item(sum),
                    None,
                    gv,
                )
                .unwrap()
                .expect("sum indexed tree entry");
            }
            assert!(verify(&db, gv).is_empty());

            plant_secondary_row_aggregate(
                &db,
                &[TEST_LEAF, b"psit"],
                IndexAxis::Sum,
                b"a",
                &make_axis_secondary_key(IndexAxis::Sum, 1, 10, b"a"),
                sum_drift.0,
                gv,
            );

            let issues = verify(&db, gv);
            let sentinel = owned(&[TEST_LEAF, b"psit", b"__psit_secondary_aggregate__"]);
            let (_, expected, actual) = issues
                .get(&sentinel)
                .unwrap_or_else(|| panic!("the sum secondary is reported: {issues:?}"));
            assert_eq!(*expected, placeholder("elements contribute count 2 sum 30"));
            assert_eq!(*actual, placeholder(sum_drift.1));
        }

        // Count and sum indexed tree: a count, a sum and an average axis.
        for (axis, sentinel, planted, contributed_label, stored_label) in [
            (
                IndexAxis::Count,
                &b"__pcpsit_count_secondary_aggregate__"[..],
                TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(1, 2),
                "elements contribute count 2 sum 2",
                "stored aggregate ProvableCountAndProvableSum(2, 3)",
            ),
            (
                IndexAxis::Sum,
                &b"__pcpsit_sum_secondary_aggregate__"[..],
                sum_drift.0,
                "elements contribute count 2 sum 30",
                sum_drift.1,
            ),
            (
                IndexAxis::Avg,
                &b"__pcpsit_avg_secondary_aggregate__"[..],
                sum_drift.0,
                "elements contribute count 2 sum 30",
                sum_drift.1,
            ),
        ] {
            let db = make_test_grovedb(gv);
            insert(
                &db,
                &[TEST_LEAF],
                b"pcpsit",
                Element::empty_provable_count_provable_sum_indexed_tree(vec![
                    (0u8, None),
                    (1u8, None),
                    (2u8, None),
                ])
                .expect("canonical axes"),
                gv,
            );
            for (key, sum) in [(b"a", 10), (b"b", 20)] {
                db.insert_into_provable_count_provable_sum_indexed_tree(
                    [TEST_LEAF, b"pcpsit"].as_ref(),
                    key,
                    Element::new_item_with_sum_item(b"v".to_vec(), sum),
                    None,
                    gv,
                )
                .unwrap()
                .expect("count and sum indexed tree entry");
            }
            assert!(verify(&db, gv).is_empty());

            plant_secondary_row_aggregate(
                &db,
                &[TEST_LEAF, b"pcpsit"],
                axis,
                b"a",
                &make_axis_secondary_key(axis, 1, 10, b"a"),
                planted,
                gv,
            );

            let issues = verify(&db, gv);
            let sentinel = owned(&[TEST_LEAF, b"pcpsit", sentinel]);
            let (_, expected, actual) = issues
                .get(&sentinel)
                .unwrap_or_else(|| panic!("the {axis:?} secondary is reported: {issues:?}"));
            assert_eq!(*expected, placeholder(contributed_label), "{axis:?}");
            assert_eq!(*actual, placeholder(stored_label), "{axis:?}");
        }
    }

    // -----------------------------------------------------------------
    // A stored aggregate that cannot be read
    // -----------------------------------------------------------------

    #[test]
    fn should_report_a_sum_tree_whose_stored_sum_cannot_be_read() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        insert(&db, &[TEST_LEAF], b"sum", Element::empty_sum_tree(), gv);
        for key in [b"a", b"b"] {
            insert(&db, &[TEST_LEAF, b"sum"], key, Element::new_sum_item(1), gv);
        }

        // One node worth i64::MAX next to one worth 1: the tree's total does
        // not fit, so its stored aggregate cannot be read.
        plant_node_aggregate_without_propagating(
            &db,
            &[TEST_LEAF, b"sum"],
            b"a",
            &Element::new_sum_item(1),
            TreeFeatureType::SummedMerkNode(i64::MAX),
            gv,
        );
        let reason = unreadable_aggregate_reason(&db, &[TEST_LEAF, b"sum"], gv);

        let issues = verify(&db, gv);
        assert_only_aggregate_issue(
            &issues,
            &[TEST_LEAF, b"sum"],
            "elements contribute count 0 sum 2",
            &format!("stored aggregate cannot be read: {reason}"),
        );
        assert_eq!(
            issues[&owned(&[TEST_LEAF, b"sum"])].0,
            [0u8; 32],
            "there is no root hash to show for the tree"
        );
    }

    #[test]
    fn should_report_a_provable_count_tree_whose_stored_count_cannot_be_read() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        insert(
            &db,
            &[TEST_LEAF],
            b"pct",
            Element::empty_provable_count_tree(),
            gv,
        );
        for key in [b"a", b"b"] {
            insert(
                &db,
                &[TEST_LEAF, b"pct"],
                key,
                Element::new_item(b"v".to_vec()),
                gv,
            );
        }
        // A tree below it, to see the walk still goes down.
        insert(
            &db,
            &[TEST_LEAF, b"pct"],
            b"sum",
            Element::empty_sum_tree(),
            gv,
        );
        insert(
            &db,
            &[TEST_LEAF, b"pct", b"sum"],
            b"x",
            Element::new_sum_item(5),
            gv,
        );
        plant_node_aggregate(
            &db,
            &[TEST_LEAF, b"pct", b"sum"],
            b"x",
            &Element::new_sum_item(5),
            TreeFeatureType::SummedMerkNode(6),
            gv,
        );

        // The root hash of a provable count tree commits to its count, so
        // hashing this tree is not possible either.
        plant_node_aggregate_without_propagating(
            &db,
            &[TEST_LEAF, b"pct"],
            b"a",
            &Element::new_item(b"v".to_vec()),
            TreeFeatureType::ProvableCountedMerkNode(u64::MAX),
            gv,
        );
        let reason = unreadable_aggregate_reason(&db, &[TEST_LEAF, b"pct"], gv);

        let issues = verify(&db, gv);
        let mut reported: Vec<_> = issues.keys().cloned().collect();
        reported.sort();
        assert_eq!(
            reported,
            vec![
                owned(&[TEST_LEAF, b"pct"]),
                owned(&[TEST_LEAF, b"pct", b"sum"]),
            ],
            "the tree and the drifted tree below it are reported: {issues:?}"
        );
        let (root, expected, actual) = issues[&owned(&[TEST_LEAF, b"pct"])];
        assert_eq!(root, [0u8; 32]);
        assert_eq!(expected, placeholder("elements contribute count 3 sum 0"));
        assert_eq!(
            actual,
            placeholder(&format!("stored aggregate cannot be read: {reason}"))
        );
    }

    #[test]
    fn should_report_an_indexed_tree_whose_primary_or_secondary_aggregate_cannot_be_read() {
        let gv = GroveVersion::latest();

        // Primary.
        {
            let db = make_test_grovedb(gv);
            make_count_indexed_tree(&db, gv);
            plant_node_aggregate_without_propagating(
                &db,
                &[TEST_LEAF, b"cidx"],
                b"a",
                &Element::new_item(b"a".to_vec()),
                TreeFeatureType::ProvableCountedMerkNode(u64::MAX),
                gv,
            );
            let reason = unreadable_aggregate_reason(&db, &[TEST_LEAF, b"cidx"], gv);

            assert_only_aggregate_issue(
                &verify(&db, gv),
                &[TEST_LEAF, b"cidx"],
                "elements contribute count 3 sum 0",
                &format!("stored aggregate cannot be read: {reason}"),
            );
        }

        // Secondary.
        {
            let db = make_test_grovedb(gv);
            make_count_indexed_tree(&db, gv);
            plant_secondary_row_aggregate(
                &db,
                &[TEST_LEAF, b"cidx"],
                IndexAxis::Count,
                b"a",
                &make_axis_secondary_key(IndexAxis::Count, 1, 0, b"a"),
                TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(u64::MAX, 1),
                gv,
            );

            let issues = verify(&db, gv);
            let sentinel = owned(&[TEST_LEAF, b"cidx", b"__cidx_secondary_aggregate__"]);
            assert_eq!(
                issues.keys().cloned().collect::<Vec<_>>(),
                vec![sentinel.clone()],
                "only the secondary is reported: {issues:?}"
            );
            let (root, expected, _) = issues[&sentinel];
            assert_eq!(root, [0u8; 32]);
            assert_eq!(expected, placeholder("elements contribute count 3 sum 3"));
        }
    }

    // -----------------------------------------------------------------
    // A stored aggregate of another kind than the tree's
    // -----------------------------------------------------------------

    /// One node of every feature type.
    fn every_feature_type() -> [TreeFeatureType; 9] {
        [
            TreeFeatureType::BasicMerkNode,
            TreeFeatureType::SummedMerkNode(1),
            TreeFeatureType::BigSummedMerkNode(1),
            TreeFeatureType::CountedMerkNode(1),
            TreeFeatureType::CountedSummedMerkNode(1, 1),
            TreeFeatureType::ProvableCountedMerkNode(1),
            TreeFeatureType::ProvableCountedSummedMerkNode(1, 1),
            TreeFeatureType::ProvableSummedMerkNode(1),
            TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(1, 1),
        ]
    }

    /// The label for a root node whose aggregate is not of the kind the
    /// tree's root hash commits to.
    fn another_kind_label(stored: AggregateData, tree_type: TreeType) -> String {
        format!(
            "stored aggregate {stored:?} is not the kind the root hash of a {tree_type:?} commits \
             to"
        )
    }

    /// The aggregate the Merk of the tree at `path` stores.
    fn stored_aggregate(db: &GroveDb, path: &[&[u8]], gv: &GroveVersion) -> AggregateData {
        let tx = db.start_transaction();
        let path: SubtreePath<&[u8]> = path.into();
        db.open_transactional_merk_at_path(path, &tx, None, gv)
            .unwrap()
            .expect("open the tree's Merk")
            .aggregate_data()
            .expect("the stored aggregate can be read")
    }

    /// The aggregate the count secondary of the indexed tree at `tree_path`
    /// stores.
    fn count_secondary_stored_aggregate(
        db: &GroveDb,
        tree_path: &[&[u8]],
        gv: &GroveVersion,
    ) -> AggregateData {
        let (tree_key, parent_path) = tree_path.split_last().expect("non-root indexed tree");
        let indexed_tree = get(db, parent_path, tree_key, gv);
        let tx = db.start_transaction();
        let path: SubtreePath<&[u8]> = tree_path.into();
        db.open_indexed_secondary_at_path(
            path,
            IndexAxis::Count,
            recorded_secondary_root_key(&indexed_tree, IndexAxis::Count),
            &tx,
            None,
            gv,
        )
        .unwrap()
        .expect("open the secondary")
        .aggregate_data()
        .expect("the stored aggregate can be read")
    }

    #[test]
    fn should_report_a_root_node_whose_aggregate_is_of_another_kind_than_its_tree() {
        let gv = GroveVersion::latest();

        // (tree, its type, the feature type its own nodes carry when the
        // root hash commits to the aggregate).
        let trees = [
            (Element::empty_tree(), TreeType::NormalTree, None),
            (Element::empty_sum_tree(), TreeType::SumTree, None),
            (Element::empty_big_sum_tree(), TreeType::BigSumTree, None),
            (Element::empty_count_tree(), TreeType::CountTree, None),
            (
                Element::empty_count_sum_tree(),
                TreeType::CountSumTree,
                None,
            ),
            (
                Element::empty_provable_count_tree(),
                TreeType::ProvableCountTree,
                Some(TreeFeatureType::ProvableCountedMerkNode(1)),
            ),
            (
                Element::empty_provable_count_sum_tree(),
                TreeType::ProvableCountSumTree,
                Some(TreeFeatureType::ProvableCountedSummedMerkNode(1, 1)),
            ),
            (
                Element::empty_provable_sum_tree(),
                TreeType::ProvableSumTree,
                Some(TreeFeatureType::ProvableSummedMerkNode(1)),
            ),
            (
                Element::empty_provable_count_provable_sum_tree(),
                TreeType::ProvableCountProvableSumTree,
                Some(TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(
                    1, 1,
                )),
            ),
        ];

        for (tree, tree_type, own_feature_type) in trees {
            for planted in every_feature_type() {
                let db = make_test_grovedb(gv);
                insert(&db, &[TEST_LEAF], b"tree", tree.clone(), gv);
                // A single element, so the planted node is the tree's root.
                insert(
                    &db,
                    &[TEST_LEAF, b"tree"],
                    b"a",
                    Element::new_item(b"v".to_vec()),
                    gv,
                );
                plant_node_aggregate_without_propagating(
                    &db,
                    &[TEST_LEAF, b"tree"],
                    b"a",
                    &Element::new_item(b"v".to_vec()),
                    planted,
                    gv,
                );

                // Whatever was planted, verification returns.
                let issues = verify(&db, gv);
                let reported = issues.get(&owned(&[TEST_LEAF, b"tree"]));
                let another_kind = placeholder(&another_kind_label(
                    stored_aggregate(&db, &[TEST_LEAF, b"tree"], gv),
                    tree_type,
                ));
                let of_another_kind =
                    own_feature_type.is_some_and(|own_feature_type| own_feature_type != planted);
                if of_another_kind {
                    let (root, _, actual) = reported.unwrap_or_else(|| {
                        panic!("{planted:?} in a {tree_type:?} is reported: {issues:?}")
                    });
                    assert_eq!(*root, [0u8; 32], "{planted:?} in a {tree_type:?}");
                    assert_eq!(*actual, another_kind, "{planted:?} in a {tree_type:?}");
                    assert_eq!(
                        issues.len(),
                        1,
                        "{planted:?} in a {tree_type:?}: {issues:?}"
                    );
                } else {
                    // The tree's own kind, or a tree whose root hash does
                    // not commit to the aggregate: the root hash is there to
                    // compute, and any drift is reported as drift.
                    assert!(
                        reported.is_none_or(|(_, _, actual)| *actual != another_kind),
                        "{planted:?} in a {tree_type:?}: {issues:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn should_report_an_indexed_tree_whose_root_aggregate_is_of_another_kind() {
        let gv = GroveVersion::latest();

        // `b` is the root of the primary of `a`, `b`, `c`, and its row the
        // root of the secondary.
        for planted in every_feature_type() {
            // Primary.
            {
                let db = make_test_grovedb(gv);
                make_count_indexed_tree(&db, gv);
                plant_node_aggregate_without_propagating(
                    &db,
                    &[TEST_LEAF, b"cidx"],
                    b"b",
                    &Element::new_item(b"b".to_vec()),
                    planted,
                    gv,
                );

                let issues = verify(&db, gv);
                if planted != TreeFeatureType::ProvableCountedMerkNode(1) {
                    assert_eq!(
                        issues.keys().cloned().collect::<Vec<_>>(),
                        vec![owned(&[TEST_LEAF, b"cidx"])],
                        "{planted:?}: {issues:?}"
                    );
                    let (root, _, actual) = issues[&owned(&[TEST_LEAF, b"cidx"])];
                    assert_eq!(root, [0u8; 32], "{planted:?}");
                    assert_eq!(
                        actual,
                        placeholder(&another_kind_label(
                            stored_aggregate(&db, &[TEST_LEAF, b"cidx"], gv),
                            TreeType::ProvableCountIndexedTree
                        )),
                        "{planted:?}"
                    );
                } else {
                    assert!(issues.is_empty(), "{planted:?}: {issues:?}");
                }
            }

            // Secondary.
            {
                let db = make_test_grovedb(gv);
                make_count_indexed_tree(&db, gv);
                plant_secondary_row_aggregate(
                    &db,
                    &[TEST_LEAF, b"cidx"],
                    IndexAxis::Count,
                    b"b",
                    &make_axis_secondary_key(IndexAxis::Count, 1, 0, b"b"),
                    planted,
                    gv,
                );

                let issues = verify(&db, gv);
                let sentinel = owned(&[TEST_LEAF, b"cidx", b"__cidx_secondary_aggregate__"]);
                if planted != TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(1, 1) {
                    assert_eq!(
                        issues.keys().cloned().collect::<Vec<_>>(),
                        vec![sentinel.clone()],
                        "{planted:?}: {issues:?}"
                    );
                    let (root, _, actual) = issues[&sentinel];
                    assert_eq!(root, [0u8; 32], "{planted:?}");
                    assert_eq!(
                        actual,
                        placeholder(&another_kind_label(
                            count_secondary_stored_aggregate(&db, &[TEST_LEAF, b"cidx"], gv),
                            TreeType::ProvableCountProvableSumTree
                        )),
                        "{planted:?}"
                    );
                } else {
                    assert!(issues.is_empty(), "{planted:?}: {issues:?}");
                }
            }
        }
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
            elements_aggregate_labels(
                &contributed,
                &Err("cannot be read: sum is overflowing".to_string())
            ),
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
