//! `ProvableCountProvableSumIndexedTree` (PCPSIT) tests.
//!
//! Phase 2/3 coverage for the multi-axis indexed tree:
//! - Empty creation across all 7 axis subsets + verify.
//! - Single + multi insert + read-back.
//! - Delete (existing + missing).
//! - Child-type rejection: count-only / sum-only items.
//! - Child-type acceptance: ItemWithSumItem, ReferenceWithSumItem,
//!   CountSumTree, ProvableCountSumTree.
//! - Error paths: wrong target, root-path, oversized key, batch
//!   non-empty / wrong axes.
//! - Aggregate (count, sum) propagation across mixed inserts and
//!   deletes.
//! - All 7 axis subsets through end-to-end insert + verify.
//! - Avg axis 0/0 invariant preserved.

#[cfg(test)]
mod tests {
    use grovedb_element::indexed::IndexAxis;
    use grovedb_version::version::GroveVersion;

    use crate::{
        batch::QualifiedGroveDbOp,
        tests::{make_test_grovedb, TEST_LEAF},
        Element, Error,
    };

    /// All 7 non-empty subsets of {Count, Sum, Avg}.
    fn all_axis_subsets() -> Vec<Vec<u8>> {
        vec![
            vec![IndexAxis::Count.tag()],
            vec![IndexAxis::Sum.tag()],
            vec![IndexAxis::Avg.tag()],
            vec![IndexAxis::Count.tag(), IndexAxis::Sum.tag()],
            vec![IndexAxis::Count.tag(), IndexAxis::Avg.tag()],
            vec![IndexAxis::Sum.tag(), IndexAxis::Avg.tag()],
            vec![
                IndexAxis::Count.tag(),
                IndexAxis::Sum.tag(),
                IndexAxis::Avg.tag(),
            ],
        ]
    }

    fn insert_empty_pcpsit(
        db: &crate::GroveDb,
        key: &[u8],
        axis_tags: &[u8],
        grove_version: &GroveVersion,
    ) {
        let axes: Vec<(u8, Option<Vec<u8>>)> = axis_tags.iter().map(|t| (*t, None)).collect();
        let elem = Element::empty_provable_count_provable_sum_indexed_tree(axes)
            .expect("axes are canonical");
        db.insert([TEST_LEAF].as_ref(), key, elem, None, None, grove_version)
            .unwrap()
            .expect("PCPSIT insertion should succeed");
        assert_verify_passes(db, grove_version);
    }

    fn assert_verify_passes(db: &crate::GroveDb, grove_version: &GroveVersion) {
        let issues = db
            .verify_grovedb(None, true, true, grove_version)
            .expect("verify_grovedb must not return a hard error");
        assert!(
            issues.is_empty(),
            "verify_grovedb reported issues: {:?}",
            issues
        );
    }

    // -----------------------------------------------------------------
    // Empty creation
    // -----------------------------------------------------------------

    #[test]
    fn pcpsit_empty_creation_verify_passes_all_axis_combinations() {
        let grove_version = GroveVersion::latest();
        for (i, tags) in all_axis_subsets().iter().enumerate() {
            let db = make_test_grovedb(grove_version);
            let key = format!("pcpsit_{}", i);
            insert_empty_pcpsit(&db, key.as_bytes(), tags, grove_version);
        }
    }

    #[test]
    fn pcpsit_empty_creation_via_batch_all_axis_combinations() {
        let grove_version = GroveVersion::latest();
        for (i, tags) in all_axis_subsets().iter().enumerate() {
            let db = make_test_grovedb(grove_version);
            let key = format!("pcpsit_{}", i);
            let axes: Vec<(u8, Option<Vec<u8>>)> = tags.iter().map(|t| (*t, None)).collect();
            let elem = Element::empty_provable_count_provable_sum_indexed_tree(axes)
                .expect("axes are canonical");
            db.apply_batch(
                vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    key.as_bytes().to_vec(),
                    elem,
                )],
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("batch PCPSIT ok");
            assert_verify_passes(&db, grove_version);
        }
    }

    // -----------------------------------------------------------------
    // Insert + delete: parameterized over axis subsets
    // -----------------------------------------------------------------

    #[test]
    fn pcpsit_single_insert_propagates_count_sum_per_subset() {
        let grove_version = GroveVersion::latest();
        for (i, tags) in all_axis_subsets().iter().enumerate() {
            let db = make_test_grovedb(grove_version);
            let key = format!("pcpsit_{}", i);
            insert_empty_pcpsit(&db, key.as_bytes(), tags, grove_version);
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, key.as_bytes()].as_ref(),
                b"row",
                Element::new_item_with_sum_item(b"v".to_vec(), 42),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
            let parent = db
                .get([TEST_LEAF].as_ref(), key.as_bytes(), None, grove_version)
                .unwrap()
                .expect("get");
            match parent {
                Element::ProvableCountProvableSumIndexedTree(_, c, s, _, _) => {
                    assert_eq!(c, 1, "axes {:?} count", tags);
                    assert_eq!(s, 42, "axes {:?} sum", tags);
                }
                other => panic!("expected PCPSIT, got {:?}", other),
            }
            assert_verify_passes(&db, grove_version);
        }
    }

    #[test]
    fn pcpsit_delete_existing_decrements_count_and_sum_per_subset() {
        let grove_version = GroveVersion::latest();
        for tags in all_axis_subsets() {
            let db = make_test_grovedb(grove_version);
            insert_empty_pcpsit(&db, b"pcpsit", &tags, grove_version);
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                b"a",
                Element::new_item_with_sum_item(b"a".to_vec(), 10),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                b"b",
                Element::new_item_with_sum_item(b"b".to_vec(), -3),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");

            let removed = db
                .delete_from_provable_count_provable_sum_indexed_tree(
                    [TEST_LEAF, b"pcpsit"].as_ref(),
                    b"a",
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("delete");
            assert!(removed);

            let parent = db
                .get([TEST_LEAF].as_ref(), b"pcpsit", None, grove_version)
                .unwrap()
                .expect("get");
            match parent {
                Element::ProvableCountProvableSumIndexedTree(_, c, s, _, _) => {
                    assert_eq!(c, 1, "axes {:?}", tags);
                    assert_eq!(s, -3, "axes {:?}", tags);
                }
                other => panic!("expected PCPSIT, got {:?}", other),
            }
            assert_verify_passes(&db, grove_version);
        }
    }

    #[test]
    fn pcpsit_delete_missing_returns_false_idempotent() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit(
            &db,
            b"pcpsit",
            &[IndexAxis::Count.tag(), IndexAxis::Sum.tag()],
            grove_version,
        );
        let removed = db
            .delete_from_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                b"absent",
                None,
                grove_version,
            )
            .unwrap()
            .expect("delete");
        assert!(!removed);
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn pcpsit_overwrite_updates_count_and_sum() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit(
            &db,
            b"pcpsit",
            &[
                IndexAxis::Count.tag(),
                IndexAxis::Sum.tag(),
                IndexAxis::Avg.tag(),
            ],
            grove_version,
        );
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"pcpsit"].as_ref(),
            b"row",
            Element::new_item_with_sum_item(b"x".to_vec(), 10),
            None,
            grove_version,
        )
        .unwrap()
        .expect("first");
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"pcpsit"].as_ref(),
            b"row",
            Element::new_item_with_sum_item(b"y".to_vec(), -2),
            None,
            grove_version,
        )
        .unwrap()
        .expect("overwrite");
        let parent = db
            .get([TEST_LEAF].as_ref(), b"pcpsit", None, grove_version)
            .unwrap()
            .expect("get");
        match parent {
            Element::ProvableCountProvableSumIndexedTree(_, 1, -2, _, _) => {}
            other => panic!("expected count=1 sum=-2, got {:?}", other),
        }
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn pcpsit_multiple_inserts_aggregate_correctly() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit(
            &db,
            b"pcpsit",
            &[
                IndexAxis::Count.tag(),
                IndexAxis::Sum.tag(),
                IndexAxis::Avg.tag(),
            ],
            grove_version,
        );
        let mut expected_count: u64 = 0;
        let mut expected_sum: i64 = 0;
        for (k, v) in [
            (b"a".as_ref(), 10),
            (b"b".as_ref(), -5),
            (b"c".as_ref(), 100),
            (b"d".as_ref(), 0),
            (b"e".as_ref(), 42),
        ] {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                k,
                Element::new_item_with_sum_item(vec![1], v),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
            expected_count += 1;
            expected_sum += v;
        }
        let parent = db
            .get([TEST_LEAF].as_ref(), b"pcpsit", None, grove_version)
            .unwrap()
            .expect("get");
        match parent {
            Element::ProvableCountProvableSumIndexedTree(_, c, s, _, _) => {
                assert_eq!(c, expected_count);
                assert_eq!(s, expected_sum);
            }
            other => panic!("expected PCPSIT, got {:?}", other),
        }
        assert_verify_passes(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // Child-type acceptance: every count-and-sum-bearing variant
    // -----------------------------------------------------------------

    #[test]
    fn pcpsit_accepts_count_sum_tree_child() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit(
            &db,
            b"pcpsit",
            &[IndexAxis::Count.tag(), IndexAxis::Sum.tag()],
            grove_version,
        );
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"pcpsit"].as_ref(),
            b"row",
            Element::empty_count_sum_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("CountSumTree insert");
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn pcpsit_accepts_provable_count_sum_tree_child() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit(
            &db,
            b"pcpsit",
            &[IndexAxis::Count.tag(), IndexAxis::Sum.tag()],
            grove_version,
        );
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"pcpsit"].as_ref(),
            b"row",
            Element::empty_provable_count_sum_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("ProvableCountSumTree insert");
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn pcpsit_accepts_provable_count_provable_sum_tree_child() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit(
            &db,
            b"pcpsit",
            &[IndexAxis::Count.tag(), IndexAxis::Sum.tag()],
            grove_version,
        );
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"pcpsit"].as_ref(),
            b"row",
            Element::empty_provable_count_provable_sum_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("ProvableCountProvableSumTree insert");
        assert_verify_passes(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // Child-type rejection
    // -----------------------------------------------------------------

    #[test]
    fn pcpsit_rejects_count_only_item() {
        // Plain Item contributes only count.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit(
            &db,
            b"pcpsit",
            &[IndexAxis::Count.tag(), IndexAxis::Sum.tag()],
            grove_version,
        );
        let result = db
            .insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                b"row",
                Element::new_item(b"x".to_vec()),
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(Error::InvalidInput(msg)) => assert!(
                msg.contains("count and sum"),
                "expected count-and-sum-bearing rejection, got: {msg}"
            ),
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }

    #[test]
    fn pcpsit_rejects_sum_only_item() {
        // Plain SumItem contributes only sum (no count-bearing role
        // beyond the implicit +1; the helper requires the "both axes"
        // shape).
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit(
            &db,
            b"pcpsit",
            &[IndexAxis::Count.tag(), IndexAxis::Sum.tag()],
            grove_version,
        );
        let result = db
            .insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                b"row",
                Element::new_sum_item(42),
                None,
                grove_version,
            )
            .unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn pcpsit_rejects_plain_tree_child() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit(
            &db,
            b"pcpsit",
            &[IndexAxis::Count.tag(), IndexAxis::Sum.tag()],
            grove_version,
        );
        let result = db
            .insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                b"row",
                Element::empty_tree(),
                None,
                grove_version,
            )
            .unwrap();
        assert!(result.is_err(), "plain Tree must be rejected");
    }

    #[test]
    fn pcpsit_rejects_sum_tree_child() {
        // SumTree is sum-bearing but not count-bearing in the joint role.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit(
            &db,
            b"pcpsit",
            &[IndexAxis::Count.tag(), IndexAxis::Sum.tag()],
            grove_version,
        );
        let result = db
            .insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                b"row",
                Element::empty_sum_tree(),
                None,
                grove_version,
            )
            .unwrap();
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------
    // Error paths: targeting / root-path / oversize / batch validation
    // -----------------------------------------------------------------

    #[test]
    fn pcpsit_insert_rejects_non_pcpsit_target() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let result = db
            .insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF].as_ref(),
                b"row",
                Element::new_item_with_sum_item(b"x".to_vec(), 1),
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(Error::InvalidPath(msg)) => assert!(
                msg.contains("ProvableCountProvableSumIndexedTree"),
                "expected PCPSIT-target rejection, got: {msg}"
            ),
            other => panic!("expected InvalidPath, got {:?}", other),
        }
    }

    #[test]
    fn pcpsit_delete_rejects_non_pcpsit_target() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let result = db
            .delete_from_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF].as_ref(),
                b"row",
                None,
                grove_version,
            )
            .unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn pcpsit_insert_at_root_path_is_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let empty_path: [&[u8]; 0] = [];
        let result = db
            .insert_into_provable_count_provable_sum_indexed_tree(
                empty_path.as_ref(),
                b"row",
                Element::new_item_with_sum_item(b"x".to_vec(), 1),
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(Error::InvalidPath(msg)) => assert!(
                msg.contains("root path"),
                "expected root-path rejection, got: {msg}"
            ),
            other => panic!("expected InvalidPath, got {:?}", other),
        }
    }

    #[test]
    fn pcpsit_delete_at_root_path_is_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let empty_path: [&[u8]; 0] = [];
        let result = db
            .delete_from_provable_count_provable_sum_indexed_tree(
                empty_path.as_ref(),
                b"row",
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(Error::InvalidPath(msg)) => assert!(
                msg.contains("root path"),
                "expected root-path rejection, got: {msg}"
            ),
            other => panic!("expected InvalidPath, got {:?}", other),
        }
    }

    #[test]
    fn pcpsit_insert_rejects_oversized_item_key() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit(
            &db,
            b"pcpsit",
            &[IndexAxis::Count.tag(), IndexAxis::Sum.tag()],
            grove_version,
        );
        let too_long = vec![0u8; 248];
        let result = db
            .insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                &too_long,
                Element::new_item_with_sum_item(b"x".to_vec(), 1),
                None,
                grove_version,
            )
            .unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn pcpsit_batch_rejects_non_empty_primary_root_key() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let bad = Element::new_provable_count_provable_sum_indexed_tree(
            Some(b"bogus".to_vec()),
            0,
            0,
            vec![(IndexAxis::Count.tag(), None)],
            None,
        )
        .expect("valid axes");
        let result = db
            .apply_batch(
                vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"pcpsit".to_vec(),
                    bad,
                )],
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert!(result.is_err(), "batch must reject non-empty PCPSIT");
    }

    #[test]
    fn pcpsit_batch_rejects_non_empty_axes_secondary_root_key() {
        // An axis slot with `secondary_root_key = Some(_)` violates the
        // empty-at-creation invariant.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let bad = Element::new_provable_count_provable_sum_indexed_tree(
            None,
            0,
            0,
            vec![(IndexAxis::Count.tag(), Some(b"bogus_sk".to_vec()))],
            None,
        )
        .expect("valid axes");
        let result = db
            .apply_batch(
                vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"pcpsit".to_vec(),
                    bad,
                )],
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            result.is_err(),
            "batch must reject PCPSIT with non-None axis secondary root key"
        );
    }

    #[test]
    fn pcpsit_batch_rejects_non_zero_count_or_sum() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let bad = Element::new_provable_count_provable_sum_indexed_tree(
            None,
            5,
            10,
            vec![(IndexAxis::Count.tag(), None), (IndexAxis::Sum.tag(), None)],
            None,
        )
        .expect("valid axes");
        let result = db
            .apply_batch(
                vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"pcpsit".to_vec(),
                    bad,
                )],
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------
    // Mixed operations + verify
    // -----------------------------------------------------------------

    #[test]
    fn pcpsit_mixed_ops_verify_passes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit(
            &db,
            b"pcpsit",
            &[
                IndexAxis::Count.tag(),
                IndexAxis::Sum.tag(),
                IndexAxis::Avg.tag(),
            ],
            grove_version,
        );
        // Mix inserts and deletes.
        for i in 0..6u8 {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                &[i],
                Element::new_item_with_sum_item(vec![i], i as i64 * 5),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        // Delete every other key.
        for i in (0..6u8).step_by(2) {
            db.delete_from_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                &[i],
                None,
                grove_version,
            )
            .unwrap()
            .expect("delete");
        }
        let parent = db
            .get([TEST_LEAF].as_ref(), b"pcpsit", None, grove_version)
            .unwrap()
            .expect("get");
        // Remaining keys: 1, 3, 5 → counts: 3, sums: 5+15+25=45.
        match parent {
            Element::ProvableCountProvableSumIndexedTree(_, 3, 45, _, _) => {}
            other => panic!("expected count=3 sum=45, got {:?}", other),
        }
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn pcpsit_avg_axis_0_over_0_empty_verify_passes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit(&db, b"pcpsit", &[IndexAxis::Avg.tag()], grove_version);
    }

    #[test]
    fn pcpsit_avg_axis_with_inserts_verify_passes() {
        // The avg axis recomputes (sum * SCALE / count) for the
        // secondary sort. After inserts the digest must stay
        // consistent — verify_grovedb is the consistency oracle.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit(&db, b"pcpsit", &[IndexAxis::Avg.tag()], grove_version);
        for (k, v) in [
            (b"a".as_ref(), 10),
            (b"b".as_ref(), 20),
            (b"c".as_ref(), 30),
        ] {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                k,
                Element::new_item_with_sum_item(vec![1], v),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        // After 3 inserts of sums (10, 20, 30): count=3, sum=60.
        let parent = db
            .get([TEST_LEAF].as_ref(), b"pcpsit", None, grove_version)
            .unwrap()
            .expect("get");
        match parent {
            Element::ProvableCountProvableSumIndexedTree(_, 3, 60, _, _) => {}
            other => panic!("expected count=3 sum=60, got {:?}", other),
        }
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn pcpsit_two_independent_pcpsits_verify_passes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit(
            &db,
            b"a",
            &[IndexAxis::Count.tag(), IndexAxis::Sum.tag()],
            grove_version,
        );
        insert_empty_pcpsit(
            &db,
            b"b",
            &[IndexAxis::Sum.tag(), IndexAxis::Avg.tag()],
            grove_version,
        );
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"a"].as_ref(),
            b"x",
            Element::new_item_with_sum_item(vec![1], 7),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"b"].as_ref(),
            b"y",
            Element::new_item_with_sum_item(vec![2], -11),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");
        assert_verify_passes(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // axes() helper + element-level introspection
    // -----------------------------------------------------------------

    #[test]
    fn pcpsit_axes_helper_returns_correct_subset_for_each_combination() {
        for tags in all_axis_subsets() {
            let axes: Vec<(u8, Option<Vec<u8>>)> = tags.iter().map(|t| (*t, None)).collect();
            let elem = Element::empty_provable_count_provable_sum_indexed_tree(axes.clone())
                .expect("axes are canonical");
            let returned = elem.axes().expect("Some(axes)");
            assert_eq!(returned.len(), tags.len());
            for (i, (t, _)) in returned.iter().enumerate() {
                assert_eq!(*t, tags[i]);
            }
        }
    }

    #[test]
    fn pcpsit_axes_helper_returns_none_for_non_pcpsit_variants() {
        let psit = Element::ProvableSumIndexedTree(None, None, 0, None);
        assert!(psit.axes().is_none());
        let pcit = Element::ProvableCountIndexedTree(None, None, 0, None);
        assert!(pcit.axes().is_none());
        let item = Element::new_item(b"x".to_vec());
        assert!(item.axes().is_none());
    }

    #[test]
    fn pcpsit_helpers_report_count_and_sum_bearing() {
        // PCPSIT itself is BOTH count-bearing and sum-bearing in the
        // joint role — so it's accepted as a child of another PCPSIT.
        let pcpsit = Element::ProvableCountProvableSumIndexedTree(
            None,
            0,
            0,
            vec![(IndexAxis::Count.tag(), None), (IndexAxis::Sum.tag(), None)],
            None,
        );
        assert!(pcpsit.is_count_and_sum_bearing_child());
        assert!(pcpsit.is_sum_bearing_child());
        assert!(pcpsit.is_indexed_tree());
    }

    // -----------------------------------------------------------------
    // Batch: fresh-create + descendant write (rejected)
    // -----------------------------------------------------------------

    // -----------------------------------------------------------------
    // Direct-API db.insert: on-disk validated path
    // -----------------------------------------------------------------

    #[test]
    fn pcpsit_direct_insert_rejects_partial_state() {
        // The direct db.insert validated path requires either fully
        // empty (primary=None, count=0, sum=0, all axis secondaries
        // None) OR fully non-empty (primary=Some). Mixing — e.g.
        // primary=None but count != 0 — must be rejected.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let bad = Element::new_provable_count_provable_sum_indexed_tree(
            None,
            5,
            0,
            vec![(IndexAxis::Count.tag(), None), (IndexAxis::Sum.tag(), None)],
            None,
        )
        .expect("valid axes");
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"pcpsit",
                bad,
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            result.is_err(),
            "PCPSIT with partial state must be rejected via direct insert"
        );
    }

    #[test]
    fn pcpsit_direct_insert_rejects_bogus_primary_root_key() {
        // db.insert with a non-empty PCPSIT claim whose primary root
        // key doesn't match an existing primary Merk should fail. The
        // outer tree-override check fires before the validated insert
        // path, but the operation must not succeed.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let bad = Element::new_provable_count_provable_sum_indexed_tree(
            Some(b"bogus_primary".to_vec()),
            5,
            10,
            vec![
                (IndexAxis::Count.tag(), Some(b"bogus_count_sec".to_vec())),
                (IndexAxis::Sum.tag(), Some(b"bogus_sum_sec".to_vec())),
            ],
            None,
        )
        .expect("valid axes");
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"pcpsit",
                bad,
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn pcpsit_batch_two_empty_pcpsits_in_one_batch_ok() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let empty_pcpsit = || {
            Element::empty_provable_count_provable_sum_indexed_tree(vec![(
                IndexAxis::Count.tag(),
                None,
            )])
            .expect("axes")
        };
        db.apply_batch(
            vec![
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"a".to_vec(),
                    empty_pcpsit(),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"b".to_vec(),
                    empty_pcpsit(),
                ),
            ],
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("two empty PCPSITs in one batch ok");
        assert_verify_passes(&db, grove_version);
    }
}
