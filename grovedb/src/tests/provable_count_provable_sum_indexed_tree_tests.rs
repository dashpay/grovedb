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

    // -----------------------------------------------------------------
    // Phase 3: indexed_count_*/indexed_sum_*/indexed_avg_* direct
    // queries over PCPSIT with various axis subsets.
    // -----------------------------------------------------------------

    /// Insert an empty PCPSIT under `[TEST_LEAF, key]` carrying all
    /// three axes (count, sum, avg). Used by per-axis direct-query
    /// tests that need every axis available.
    fn insert_empty_pcpsit_all_axes(db: &crate::GroveDb, key: &[u8], grove_version: &GroveVersion) {
        insert_empty_pcpsit(
            db,
            key,
            &[
                IndexAxis::Count.tag(),
                IndexAxis::Sum.tag(),
                IndexAxis::Avg.tag(),
            ],
            grove_version,
        );
    }

    /// Insert a CountSumTree child under `[TEST_LEAF, indexed_key]` whose
    /// `(count, sum)` pair is DERIVED rather than caller-asserted.
    ///
    /// The child enters the index EMPTY and is then populated with `count`
    /// sum items whose values total `sum` (the whole sum is carried on the
    /// first item, the remainder are zero-valued). A CountSumTree counts
    /// every element and sums every sum item, so propagation derives
    /// exactly `(count, sum)` and feeds it to all three secondary axes.
    ///
    /// `count == 0` cannot carry a non-zero `sum` — there is no element to
    /// hold it — which is the same constraint the production rule enforces.
    fn pcpsit_insert_count_sum_child(
        db: &crate::GroveDb,
        indexed_key: &[u8],
        item_key: &[u8],
        count: u64,
        sum: i64,
        grove_version: &GroveVersion,
    ) {
        assert!(
            count > 0 || sum == 0,
            "a zero-count child has no element to derive a non-zero sum from"
        );
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, indexed_key].as_ref(),
            item_key,
            Element::empty_count_sum_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert empty count-sum-tree child");
        for i in 0..count {
            db.insert(
                [TEST_LEAF, indexed_key, item_key].as_ref(),
                &i.to_be_bytes(),
                Element::new_sum_item(if i == 0 { sum } else { 0 }),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("populate count-sum-tree child so count and sum are derived");
        }
    }

    /// Populate `[TEST_LEAF, "pcpsit"]` with CountSumTree children whose
    /// derived (count, sum) values give distinct secondary keys across all
    /// three axes. Each tuple is `(item_key, count, sum)`; the avg axis
    /// encodes `floor(sum * 10^19 / count)`.
    fn pcpsit_populate_count_sum_dataset(
        db: &crate::GroveDb,
        grove_version: &GroveVersion,
    ) -> Vec<(Vec<u8>, u64, i64)> {
        let dataset = vec![
            (b"alice".to_vec(), 2u64, 10i64),  // avg = 5
            (b"bob".to_vec(), 4u64, 100i64),   // avg = 25
            (b"carol".to_vec(), 5u64, -25i64), // avg = -5
            (b"dave".to_vec(), 1u64, 0i64),    // avg = 0
            (b"eve".to_vec(), 3u64, 9i64),     // avg = 3
        ];
        for (k, c, s) in &dataset {
            pcpsit_insert_count_sum_child(db, b"pcpsit", k, *c, *s, grove_version);
        }
        dataset
    }

    /// SCALE used by the avg-axis encoding (10^19).
    const AVG_SCALE: i128 = grovedb_element::indexed::AVG_FIXED_POINT_SCALE;

    // ---- indexed_count_* over PCPSIT (count axis in TLV) ----

    #[test]
    fn pcpsit_indexed_count_top_k_returns_highest_count_first() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit_all_axes(&db, b"pcpsit", grove_version);
        pcpsit_populate_count_sum_dataset(&db, grove_version);

        // Counts: alice=2, bob=4, carol=5, dave=1, eve=3.
        // Descending: carol(5), bob(4), eve(3).
        let top3 = db
            .indexed_count_top_k(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                3,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("top-k count");
        assert_eq!(
            top3,
            vec![
                (5u64, b"carol".to_vec()),
                (4, b"bob".to_vec()),
                (3, b"eve".to_vec()),
            ]
        );
    }

    #[test]
    fn pcpsit_indexed_count_range_and_aggregate() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit_all_axes(&db, b"pcpsit", grove_version);
        pcpsit_populate_count_sum_dataset(&db, grove_version);

        // Range [2, 4]: alice(2), eve(3), bob(4).
        let in_range = db
            .indexed_count_range(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                2,
                4,
                false,
                100,
                None,
                grove_version,
            )
            .unwrap()
            .expect("range");
        assert_eq!(
            in_range,
            vec![
                (2u64, b"alice".to_vec()),
                (3, b"eve".to_vec()),
                (4, b"bob".to_vec()),
            ]
        );

        // Aggregate count [2, 4]: 3 entries.
        let agg = db
            .indexed_count_aggregate_over_value_range(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                2,
                4,
                None,
                grove_version,
            )
            .unwrap()
            .expect("agg");
        assert_eq!(agg, 3);

        // Aggregate full scan = 5.
        let total = db
            .indexed_count_aggregate_over_value_range(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                0,
                u64::MAX,
                None,
                grove_version,
            )
            .unwrap()
            .expect("total");
        assert_eq!(total, 5);
    }

    // ---- indexed_sum_* over PCPSIT (sum axis in TLV) ----

    #[test]
    fn pcpsit_indexed_sum_top_k_returns_highest_sum_first() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit_all_axes(&db, b"pcpsit", grove_version);
        pcpsit_populate_count_sum_dataset(&db, grove_version);

        // Sums: alice=10, bob=100, carol=-25, dave=0, eve=9.
        // Descending: bob(100), alice(10), eve(9).
        let top3 = db
            .indexed_sum_top_k(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                3,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("top-k sum");
        assert_eq!(
            top3,
            vec![
                (100i64, b"bob".to_vec()),
                (10, b"alice".to_vec()),
                (9, b"eve".to_vec()),
            ]
        );

        // Ascending: carol(-25), dave(0), eve(9).
        let asc3 = db
            .indexed_sum_top_k(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                3,
                false,
                None,
                grove_version,
            )
            .unwrap()
            .expect("asc");
        assert_eq!(
            asc3,
            vec![
                (-25i64, b"carol".to_vec()),
                (0, b"dave".to_vec()),
                (9, b"eve".to_vec()),
            ]
        );
    }

    #[test]
    fn pcpsit_indexed_sum_range_and_aggregate() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit_all_axes(&db, b"pcpsit", grove_version);
        pcpsit_populate_count_sum_dataset(&db, grove_version);

        // Range [0, 10] ascending: dave(0), eve(9), alice(10).
        let in_range = db
            .indexed_sum_range(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                0,
                10,
                false,
                100,
                None,
                grove_version,
            )
            .unwrap()
            .expect("sum range");
        assert_eq!(
            in_range,
            vec![
                (0i64, b"dave".to_vec()),
                (9, b"eve".to_vec()),
                (10, b"alice".to_vec()),
            ]
        );

        // Aggregate sum in [0, 10]: 0 + 9 + 10 = 19.
        let agg = db
            .indexed_sum_aggregate_over_value_range(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                0,
                10,
                None,
                grove_version,
            )
            .unwrap()
            .expect("agg");
        assert_eq!(agg, 19);

        // Aggregate full sum: -25 + 0 + 9 + 10 + 100 = 94.
        let total = db
            .indexed_sum_aggregate_over_value_range(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                i64::MIN,
                i64::MAX,
                None,
                grove_version,
            )
            .unwrap()
            .expect("total");
        assert_eq!(total, 94);
    }

    // ---- indexed_avg_* over PCPSIT (avg axis in TLV) ----

    #[test]
    fn pcpsit_indexed_avg_top_k_orders_by_floor_sum_div_count() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit_all_axes(&db, b"pcpsit", grove_version);
        pcpsit_populate_count_sum_dataset(&db, grove_version);

        // Avg fixed-point (× 10^19):
        //   alice  (2, 10)   →  5 * SCALE
        //   bob    (4, 100)  → 25 * SCALE
        //   carol  (5, -25)  → -5 * SCALE
        //   dave   (1,   0)  →  0 * SCALE
        //   eve    (3,   9)  →  3 * SCALE
        // Descending: bob(25), alice(5), eve(3).
        let top3 = db
            .indexed_avg_top_k(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                3,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("top-k avg");
        assert_eq!(
            top3,
            vec![
                (25 * AVG_SCALE, b"bob".to_vec()),
                (5 * AVG_SCALE, b"alice".to_vec()),
                (3 * AVG_SCALE, b"eve".to_vec()),
            ]
        );

        // Ascending: carol(-5), dave(0), eve(3).
        let asc3 = db
            .indexed_avg_top_k(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                3,
                false,
                None,
                grove_version,
            )
            .unwrap()
            .expect("asc avg");
        assert_eq!(
            asc3,
            vec![
                (-5 * AVG_SCALE, b"carol".to_vec()),
                (0, b"dave".to_vec()),
                (3 * AVG_SCALE, b"eve".to_vec()),
            ]
        );
    }

    #[test]
    fn pcpsit_indexed_avg_range_filters_inclusive_bounds() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit_all_axes(&db, b"pcpsit", grove_version);
        pcpsit_populate_count_sum_dataset(&db, grove_version);

        // Range [0, 5*SCALE] ascending: dave(0), eve(3), alice(5).
        let in_range = db
            .indexed_avg_range(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                0,
                5 * AVG_SCALE,
                false,
                100,
                None,
                grove_version,
            )
            .unwrap()
            .expect("avg range");
        assert_eq!(
            in_range,
            vec![
                (0i128, b"dave".to_vec()),
                (3 * AVG_SCALE, b"eve".to_vec()),
                (5 * AVG_SCALE, b"alice".to_vec()),
            ]
        );

        // Exact-match: [3*SCALE, 3*SCALE] → eve.
        let exact = db
            .indexed_avg_range(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                3 * AVG_SCALE,
                3 * AVG_SCALE,
                false,
                100,
                None,
                grove_version,
            )
            .unwrap()
            .expect("exact");
        assert_eq!(exact, vec![(3 * AVG_SCALE, b"eve".to_vec())]);

        // lo > hi: empty.
        let empty = db
            .indexed_avg_range(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                100 * AVG_SCALE,
                10 * AVG_SCALE,
                false,
                100,
                None,
                grove_version,
            )
            .unwrap()
            .expect("lo>hi");
        assert!(empty.is_empty());

        // Full scan: 5 entries.
        let full = db
            .indexed_avg_range(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                i128::MIN,
                i128::MAX,
                false,
                100,
                None,
                grove_version,
            )
            .unwrap()
            .expect("full");
        assert_eq!(full.len(), 5);
    }

    #[test]
    fn pcpsit_indexed_avg_top_k_paginated_pages_through_dataset() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit_all_axes(&db, b"pcpsit", grove_version);
        pcpsit_populate_count_sum_dataset(&db, grove_version);

        // Descending avg order: bob(25), alice(5), eve(3), dave(0),
        // carol(-5).
        // Page 1 (offset=0, k=2): bob, alice.
        let page1 = db
            .indexed_avg_top_k_paginated(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                2,
                0,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("page 1");
        assert_eq!(
            page1.entries,
            vec![
                (25 * AVG_SCALE, b"bob".to_vec()),
                (5 * AVG_SCALE, b"alice".to_vec()),
            ]
        );

        // Page 2 (offset=2, k=2): eve, dave.
        let page2 = db
            .indexed_avg_top_k_paginated(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                2,
                2,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("page 2");
        assert_eq!(
            page2.entries,
            vec![(3 * AVG_SCALE, b"eve".to_vec()), (0, b"dave".to_vec())]
        );

        // Offset beyond end.
        let beyond = db
            .indexed_avg_top_k_paginated(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                5,
                100,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("beyond");
        assert!(beyond.entries.is_empty());
    }

    #[test]
    fn pcpsit_indexed_avg_zero_over_zero_invariant() {
        // 0/0 must produce avg=0 (matches compute_avg_fixed_point's
        // documented behavior). With our dataset, dave (1, 0) directly
        // tests the 0 sum case; for an actual 0/0 we'd need a child
        // contributing (0, 0) — not constructible at the PCPSIT entry
        // level, since the parent count goes up by the child's count
        // contribution. So we settle for verifying that a 0-sum entry
        // (dave) maps to avg 0 in the encoded secondary.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit_all_axes(&db, b"pcpsit", grove_version);
        pcpsit_populate_count_sum_dataset(&db, grove_version);

        // dave has (1, 0) → avg=0.
        let zero_only = db
            .indexed_avg_range(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                0,
                0,
                false,
                100,
                None,
                grove_version,
            )
            .unwrap()
            .expect("zero only");
        assert_eq!(zero_only, vec![(0i128, b"dave".to_vec())]);
    }

    #[test]
    fn pcpsit_indexed_avg_same_avg_different_keys_breaks_tie_by_key() {
        // Two entries with identical avg sort by original_key
        // ascending in the secondary's key space.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit_all_axes(&db, b"pcpsit", grove_version);

        // Both (1, 7) and (2, 14) yield avg = 7 * SCALE. Each child is
        // inserted empty and populated so its (count, sum) is derived.
        for (k, c, s) in [(b"aaa".as_ref(), 1u64, 7i64), (b"zzz", 2, 14)] {
            pcpsit_insert_count_sum_child(&db, b"pcpsit", k, c, s, grove_version);
        }

        let asc = db
            .indexed_avg_top_k(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                10,
                false,
                None,
                grove_version,
            )
            .unwrap()
            .expect("asc");
        // Both share avg = 7*SCALE; tie-break by original_key ascending.
        assert_eq!(
            asc,
            vec![
                (7 * AVG_SCALE, b"aaa".to_vec()),
                (7 * AVG_SCALE, b"zzz".to_vec()),
            ]
        );
    }

    // ---- axis-compatibility rejection on PCPSIT subsets ----

    #[test]
    fn pcpsit_indexed_count_top_k_on_sum_only_pcpsit_rejects() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit(&db, b"pcpsit", &[IndexAxis::Sum.tag()], grove_version);
        let result = db
            .indexed_count_top_k(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                5,
                true,
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(Error::InvalidPath(msg)) => {
                assert!(
                    msg.contains("Count") && msg.contains("not indexed"),
                    "expected count-axis rejection on sum-only PCPSIT, got: {msg}"
                );
            }
            other => panic!("expected InvalidPath, got {:?}", other),
        }
    }

    #[test]
    fn pcpsit_indexed_sum_top_k_on_count_only_pcpsit_rejects() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit(&db, b"pcpsit", &[IndexAxis::Count.tag()], grove_version);
        let result = db
            .indexed_sum_top_k(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                5,
                true,
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(Error::InvalidPath(msg)) => {
                assert!(
                    msg.contains("Sum") && msg.contains("not indexed"),
                    "expected sum-axis rejection on count-only PCPSIT, got: {msg}"
                );
            }
            other => panic!("expected InvalidPath, got {:?}", other),
        }
    }

    #[test]
    fn pcpsit_indexed_avg_top_k_on_count_sum_pcpsit_rejects() {
        // PCPSIT with {Count, Sum} but NO avg → indexed_avg_* rejects.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit(
            &db,
            b"pcpsit",
            &[IndexAxis::Count.tag(), IndexAxis::Sum.tag()],
            grove_version,
        );
        let result = db
            .indexed_avg_top_k(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                5,
                true,
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(Error::InvalidPath(msg)) => {
                assert!(
                    msg.contains("Avg") && msg.contains("not indexed"),
                    "expected avg-axis rejection on count+sum PCPSIT, got: {msg}"
                );
            }
            other => panic!("expected InvalidPath, got {:?}", other),
        }
    }

    #[test]
    fn pcpsit_indexed_count_top_k_on_all_axes_succeeds_after_insert() {
        // Sanity smoke test: with all three axes configured, every
        // family must succeed (no rejection).
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit_all_axes(&db, b"pcpsit", grove_version);
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"pcpsit"].as_ref(),
            b"row",
            Element::new_item_with_sum_item(b"v".to_vec(), 17),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");

        // All three families produce results.
        let by_count = db
            .indexed_count_top_k(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                1,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("count");
        assert_eq!(by_count, vec![(1u64, b"row".to_vec())]);
        let by_sum = db
            .indexed_sum_top_k(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                1,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("sum");
        assert_eq!(by_sum, vec![(17i64, b"row".to_vec())]);
        let by_avg = db
            .indexed_avg_top_k(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                1,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("avg");
        // avg = floor(17 * SCALE / 1) = 17 * SCALE.
        assert_eq!(by_avg, vec![(17 * AVG_SCALE, b"row".to_vec())]);
    }

    // -----------------------------------------------------------------
    // Depth > 1 propagation
    // -----------------------------------------------------------------

    #[test]
    fn pcpsit_depth_2_under_tree_propagates_count_and_sum() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"parent",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("parent");
        let axes = vec![(IndexAxis::Count.tag(), None), (IndexAxis::Sum.tag(), None)];
        db.insert(
            [TEST_LEAF, b"parent"].as_ref(),
            b"pcpsit",
            Element::empty_provable_count_provable_sum_indexed_tree(axes).unwrap(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("pcpsit");
        for (k, v) in [(b"a".as_ref(), 5i64), (b"b", 15), (b"c", -3)] {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"parent", b"pcpsit"].as_ref(),
                k,
                Element::new_item_with_sum_item(k.to_vec(), v),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        assert_verify_passes(&db, grove_version);
        let elem = db
            .get(
                [TEST_LEAF, b"parent"].as_ref(),
                b"pcpsit",
                None,
                grove_version,
            )
            .unwrap()
            .expect("get");
        match elem.underlying() {
            Element::ProvableCountProvableSumIndexedTree(_, c, s, _, _) => {
                assert_eq!(*c, 3);
                assert_eq!(*s, 17);
            }
            other => panic!("expected PCPSIT, got {:?}", other),
        }
    }

    #[test]
    fn pcpsit_depth_3_propagates_aggregates() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"l1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("l1");
        db.insert(
            [TEST_LEAF, b"l1"].as_ref(),
            b"l2",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("l2");
        let axes = vec![
            (IndexAxis::Count.tag(), None),
            (IndexAxis::Sum.tag(), None),
            (IndexAxis::Avg.tag(), None),
        ];
        db.insert(
            [TEST_LEAF, b"l1", b"l2"].as_ref(),
            b"pcpsit",
            Element::empty_provable_count_provable_sum_indexed_tree(axes).unwrap(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("pcpsit");
        for (k, v) in [(b"a".as_ref(), 4i64), (b"b", 8)] {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"l1", b"l2", b"pcpsit"].as_ref(),
                k,
                Element::new_item_with_sum_item(k.to_vec(), v),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        assert_verify_passes(&db, grove_version);
        let elem = db
            .get(
                [TEST_LEAF, b"l1", b"l2"].as_ref(),
                b"pcpsit",
                None,
                grove_version,
            )
            .unwrap()
            .expect("get");
        match elem.underlying() {
            Element::ProvableCountProvableSumIndexedTree(_, c, s, _, _) => {
                assert_eq!(*c, 2);
                assert_eq!(*s, 12);
            }
            other => panic!("expected PCPSIT, got {:?}", other),
        }
    }

    #[test]
    fn pcpsit_delete_then_reinsert_at_depth_2() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"parent",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("parent");
        let axes = vec![(IndexAxis::Count.tag(), None), (IndexAxis::Sum.tag(), None)];
        db.insert(
            [TEST_LEAF, b"parent"].as_ref(),
            b"pcpsit",
            Element::empty_provable_count_provable_sum_indexed_tree(axes).unwrap(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("pcpsit");
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"parent", b"pcpsit"].as_ref(),
            b"a",
            Element::new_item_with_sum_item(b"a".to_vec(), 10),
            None,
            grove_version,
        )
        .unwrap()
        .expect("a");
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"parent", b"pcpsit"].as_ref(),
            b"b",
            Element::new_item_with_sum_item(b"b".to_vec(), 20),
            None,
            grove_version,
        )
        .unwrap()
        .expect("b");
        db.delete_from_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"parent", b"pcpsit"].as_ref(),
            b"a",
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete");
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"parent", b"pcpsit"].as_ref(),
            b"c",
            Element::new_item_with_sum_item(b"c".to_vec(), 5),
            None,
            grove_version,
        )
        .unwrap()
        .expect("c");
        assert_verify_passes(&db, grove_version);
        let elem = db
            .get(
                [TEST_LEAF, b"parent"].as_ref(),
                b"pcpsit",
                None,
                grove_version,
            )
            .unwrap()
            .expect("get");
        match elem.underlying() {
            Element::ProvableCountProvableSumIndexedTree(_, c, s, _, _) => {
                assert_eq!(*c, 2);
                assert_eq!(*s, 25);
            }
            other => panic!("expected PCPSIT, got {:?}", other),
        }
    }

    /// Security regression (P1): the dedicated PCPSIT insert
    /// short-circuits child subtree roots to NULL_HASH, so it must
    /// reject a non-empty `CountSumTree(Some(root_key), ..)` child claim
    /// — otherwise the serialized element persists a root_key that
    /// disagrees with the empty merk node it is bound to.
    #[test]
    fn pcpsit_rejects_non_empty_count_sum_tree_child() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        insert_empty_pcpsit(
            &db,
            b"pcpsit",
            &[IndexAxis::Count.tag(), IndexAxis::Sum.tag()],
            v,
        );

        // A CountSumTree claiming a non-empty root must be rejected.
        let res = db
            .insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                b"k",
                Element::CountSumTree(Some(vec![7u8; 32]), 0, 0, None),
                None,
                v,
            )
            .unwrap();
        assert!(
            matches!(res, Err(Error::NotSupported(_))),
            "non-empty CountSumTree child must be rejected by the dedicated PCPSIT insert \
             guard; got {res:?}"
        );

        // An EMPTY CountSumTree child is accepted.
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"pcpsit"].as_ref(),
            b"k",
            Element::CountSumTree(None, 0, 0, None),
            None,
            v,
        )
        .unwrap()
        .expect("empty CountSumTree child accepted");
        assert_verify_passes(&db, v);
    }

    /// Security regression (P1): an avg-configured PCPSIT prepends a
    /// 16-byte fixed-point average to its secondary key, so item keys
    /// must be capped at 239 bytes (16 + 239 = 255, Merk's ceiling). A
    /// 240-byte key would build a 256-byte avg-secondary key, exceeding
    /// the limit (a silent corruption in release builds).
    #[test]
    fn pcpsit_avg_axis_enforces_239_byte_item_key_limit() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        insert_empty_pcpsit(&db, b"pcpsit", &[IndexAxis::Avg.tag()], v);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcpsit"];

        // 240-byte key → rejected (avg secondary key would be 256 bytes).
        let key_240 = vec![b'k'; 240];
        let res = db
            .insert_into_provable_count_provable_sum_indexed_tree(
                path,
                &key_240,
                Element::new_item_with_sum_item(vec![1], 5),
                None,
                v,
            )
            .unwrap();
        assert!(
            matches!(res, Err(Error::InvalidInput(_))),
            "240-byte item key under an Avg axis must be rejected; got {res:?}"
        );

        // 239-byte key → accepted (avg secondary key = 255 bytes exactly).
        let key_239 = vec![b'k'; 239];
        db.insert_into_provable_count_provable_sum_indexed_tree(
            path,
            &key_239,
            Element::new_item_with_sum_item(vec![1], 5),
            None,
            v,
        )
        .unwrap()
        .expect("239-byte item key under an Avg axis accepted");
        assert_verify_passes(&db, v);
    }

    /// A count/sum-only PCPSIT (no avg axis) keeps the 247-byte item-key
    /// limit (8-byte sort-key prefix), unaffected by the avg tightening.
    #[test]
    fn pcpsit_count_sum_only_keeps_247_byte_item_key_limit() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        insert_empty_pcpsit(
            &db,
            b"pcpsit",
            &[IndexAxis::Count.tag(), IndexAxis::Sum.tag()],
            v,
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"pcpsit"];

        let key_247 = vec![b'k'; 247];
        db.insert_into_provable_count_provable_sum_indexed_tree(
            path,
            &key_247,
            Element::new_item_with_sum_item(vec![1], 5),
            None,
            v,
        )
        .unwrap()
        .expect("247-byte item key accepted for count/sum-only PCPSIT");

        let key_248 = vec![b'k'; 248];
        let res = db
            .insert_into_provable_count_provable_sum_indexed_tree(
                path,
                &key_248,
                Element::new_item_with_sum_item(vec![1], 5),
                None,
                v,
            )
            .unwrap();
        assert!(
            matches!(res, Err(Error::InvalidInput(_))),
            "248-byte item key must be rejected even without an avg axis; got {res:?}"
        );
        assert_verify_passes(&db, v);
    }

    /// Security regression (P2): the direct empty-insert path must reject
    /// non-canonical axes. The `Element` enum is public, so a caller can
    /// build a PCPSIT with unsorted / duplicate / empty / unknown-tag
    /// axes that the validating constructor would have refused; the
    /// insert path must apply the same canonical-axes check.
    #[test]
    fn pcpsit_direct_empty_insert_rejects_non_canonical_axes() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        // Each malformed axes TLV, constructed directly (bypassing the
        // validating constructor).
        let cases: Vec<(&str, Vec<(u8, Option<Vec<u8>>)>)> = vec![
            (
                "unsorted",
                vec![(IndexAxis::Sum.tag(), None), (IndexAxis::Count.tag(), None)],
            ),
            (
                "duplicate",
                vec![
                    (IndexAxis::Count.tag(), None),
                    (IndexAxis::Count.tag(), None),
                ],
            ),
            ("empty", vec![]),
            ("unknown_tag", vec![(99u8, None)]),
        ];
        for (i, (label, axes)) in cases.into_iter().enumerate() {
            let key = format!("bad_{i}");
            let bad = Element::ProvableCountProvableSumIndexedTree(None, 0, 0, axes, None);
            let res = db
                .insert([TEST_LEAF].as_ref(), key.as_bytes(), bad, None, None, v)
                .unwrap();
            assert!(
                res.is_err(),
                "direct empty insert of PCPSIT with {label} axes must be rejected; got {res:?}"
            );
        }
    }

    /// Direct `db.insert` of a NON-EMPTY PCPSIT must validate the
    /// claimed primary/axis-secondary root keys against on-disk state and
    /// succeed when they match. Exercises the non-empty success path
    /// (open each axis secondary, compare root keys, recompute
    /// axes_digest). We populate a PCPSIT, read back its element (which
    /// now carries real primary + per-axis secondary root keys), and
    /// re-insert that exact element.
    #[test]
    fn pcpsit_direct_insert_non_empty_with_matching_roots_succeeds() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        insert_empty_pcpsit(
            &db,
            b"pcpsit",
            &[IndexAxis::Count.tag(), IndexAxis::Sum.tag()],
            v,
        );
        for (k, s) in [(b"a".as_ref(), 10i64), (b"b", 20)] {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                k,
                Element::new_item_with_sum_item(k.to_vec(), s),
                None,
                v,
            )
            .unwrap()
            .expect("populate");
        }
        // Read back the populated element — it now carries Some(primary),
        // non-zero count/sum, and per-axis Some(secondary_root_key).
        let populated = db
            .get([TEST_LEAF].as_ref(), b"pcpsit", None, v)
            .unwrap()
            .expect("get populated PCPSIT");
        assert!(matches!(
            populated,
            Element::ProvableCountProvableSumIndexedTree(Some(_), 2, 30, _, _)
        ));
        // Re-insert the exact element via the generic db.insert — the
        // non-empty validation opens each child merk and compares roots.
        // Override must be allowed since the key already holds the PCPSIT.
        let opts = crate::operations::insert::InsertOptions {
            validate_insertion_does_not_override: false,
            validate_insertion_does_not_override_tree: false,
            base_root_storage_is_free: true,
        };
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            populated,
            Some(opts),
            None,
            v,
        )
        .unwrap()
        .expect("re-insert non-empty PCPSIT with matching roots");
        assert_verify_passes(&db, v);
    }

    /// Direct `db.insert` of a non-empty PCPSIT with a mismatched axis
    /// secondary root key must be rejected.
    #[test]
    fn pcpsit_direct_insert_non_empty_with_mismatched_axis_root_rejected() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        insert_empty_pcpsit(
            &db,
            b"pcpsit",
            &[IndexAxis::Count.tag(), IndexAxis::Sum.tag()],
            v,
        );
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"pcpsit"].as_ref(),
            b"a",
            Element::new_item_with_sum_item(b"a".to_vec(), 10),
            None,
            v,
        )
        .unwrap()
        .expect("populate");
        let populated = db
            .get([TEST_LEAF].as_ref(), b"pcpsit", None, v)
            .unwrap()
            .expect("get");
        let Element::ProvableCountProvableSumIndexedTree(primary, count, sum, mut axes, flags) =
            populated
        else {
            panic!("expected PCPSIT");
        };
        // Corrupt the first axis's secondary root key.
        axes[0].1 = Some(vec![0xAB; 32]);
        let tampered =
            Element::ProvableCountProvableSumIndexedTree(primary, count, sum, axes, flags);
        let opts = crate::operations::insert::InsertOptions {
            validate_insertion_does_not_override: false,
            validate_insertion_does_not_override_tree: false,
            base_root_storage_is_free: true,
        };
        let res = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"pcpsit",
                tampered,
                Some(opts),
                None,
                v,
            )
            .unwrap();
        assert!(
            matches!(res, Err(Error::InvalidInput(_))),
            "mismatched axis secondary root key must be rejected; got {res:?}"
        );
    }

    /// Batch analogue of the above: the batch empty-creation path must
    /// also reject non-canonical axes (it previously checked only the
    /// 1..=3 count, not sortedness / duplicates / tag validity).
    #[test]
    fn pcpsit_batch_empty_insert_rejects_non_canonical_axes() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        let bad = Element::ProvableCountProvableSumIndexedTree(
            None,
            0,
            0,
            vec![(IndexAxis::Sum.tag(), None), (IndexAxis::Count.tag(), None)],
            None,
        );
        let res = db
            .apply_batch(
                vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"pcpsit".to_vec(),
                    bad,
                )],
                None,
                None,
                v,
            )
            .unwrap();
        assert!(
            res.is_err(),
            "batch empty insert of PCPSIT with unsorted axes must be rejected; got {res:?}"
        );
    }

    // -----------------------------------------------------------------
    // V1 proof regression tests (PR #657 BUG 1 + BUG 2)
    //
    // Before the fix an empty PCPSIT selected by a V1 proof was rejected
    // ("V1 empty tree value hash mismatch") because the verifier used the
    // two-input combine_hash instead of combine_hash_three(H(value),
    // NULL_HASH, axes_digest(zero_axes)) (BUG 2). A non-empty PCPSIT
    // crossed by a subquery was silently dropped by the prover and the
    // verifier rejected the descent as "Phase 2" NotSupported (BUG 1).
    // These lock the fixed behavior.
    // -----------------------------------------------------------------

    fn pcpsit_key_query(key: &[u8]) -> crate::PathQuery {
        use crate::{query::SizedQuery, PathQuery, QueryItem, SubqueryBranch};
        PathQuery {
            path: vec![TEST_LEAF.to_vec()],
            query: SizedQuery {
                query: grovedb_merk::proofs::Query {
                    items: vec![QueryItem::Key(key.to_vec())],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: None,
                        subquery: None,
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                    add_parent_tree_on_subquery: false,
                    read_mode: None,
                },
                limit: None,
                offset: None,
            },
        }
    }

    fn pcpsit_key_subquery(key: &[u8]) -> crate::PathQuery {
        use crate::{query::SizedQuery, PathQuery, QueryItem, SubqueryBranch};
        use grovedb_merk::proofs::Query;
        let mut inner = Query::new();
        inner.insert_all();
        PathQuery {
            path: vec![TEST_LEAF.to_vec()],
            query: SizedQuery {
                query: grovedb_merk::proofs::Query {
                    items: vec![QueryItem::Key(key.to_vec())],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: None,
                        subquery: Some(inner.into()),
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                    add_parent_tree_on_subquery: false,
                    read_mode: None,
                },
                limit: None,
                offset: None,
            },
        }
    }

    #[test]
    fn pcpsit_empty_v1_proof_terminal_verifies_all_axis_subsets() {
        // BUG 2: an empty PCPSIT terminal proof must verify against
        // combine_hash_three(H(value), NULL_HASH, axes_digest(zero_axes))
        // for every axis subset — the digest depends on the axes list.
        let v = GroveVersion::latest();
        for axes in all_axis_subsets() {
            let db = make_test_grovedb(v);
            insert_empty_pcpsit(&db, b"pcp", &axes, v);
            let pq = pcpsit_key_query(b"pcp");
            let proof = db
                .prove_query(&pq, None, v)
                .unwrap()
                .expect("prove empty pcpsit");
            let (root, results) = crate::GroveDb::verify_query(&proof, &pq, v)
                .unwrap_or_else(|e| panic!("verify empty pcpsit axes {axes:?}: {e:?}"));
            assert_eq!(root, db.root_hash(None, v).unwrap().expect("root"));
            assert_eq!(results.len(), 1, "empty PCPSIT is a single terminal result");
        }
    }

    #[test]
    fn pcpsit_non_empty_v1_subquery_verifies() {
        // BUG 1: a non-empty PCPSIT crossed by a subquery must descend
        // into the primary and verify via combine_hash_three(H(value),
        // primary_root, axes_digest).
        let v = GroveVersion::latest();
        let axes = vec![
            IndexAxis::Count.tag(),
            IndexAxis::Sum.tag(),
            IndexAxis::Avg.tag(),
        ];
        let db = make_test_grovedb(v);
        insert_empty_pcpsit(&db, b"pcp", &axes, v);
        for (k, val, s) in [
            (b"a".as_ref(), b"x".to_vec(), 10i64),
            (b"b", b"y".to_vec(), -3),
            (b"c", b"z".to_vec(), 7),
        ] {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcp"].as_ref(),
                k,
                Element::new_item_with_sum_item(val, s),
                None,
                v,
            )
            .unwrap()
            .expect("populate pcpsit");
        }
        let pq = pcpsit_key_subquery(b"pcp");
        let proof = db
            .prove_query(&pq, None, v)
            .unwrap()
            .expect("prove pcpsit subquery");
        let (root, results) =
            crate::GroveDb::verify_query(&proof, &pq, v).expect("verify pcpsit subquery");
        assert_eq!(root, db.root_hash(None, v).unwrap().expect("root"));
        assert_eq!(
            results.len(),
            3,
            "subquery must return all three PCPSIT rows"
        );
    }

    #[test]
    fn pcpsit_non_empty_v1_subquery_single_axis_verifies() {
        // Cover the single-axis case too — axes_digest over one entry has
        // a distinct payload length from the three-axis case.
        let v = GroveVersion::latest();
        let axes = vec![IndexAxis::Sum.tag()];
        let db = make_test_grovedb(v);
        insert_empty_pcpsit(&db, b"pcp", &axes, v);
        for (k, s) in [(b"a".as_ref(), 5i64), (b"b", -2), (b"c", 11), (b"d", 0)] {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcp"].as_ref(),
                k,
                Element::new_item_with_sum_item(b"v".to_vec(), s),
                None,
                v,
            )
            .unwrap()
            .expect("populate pcpsit");
        }
        let pq = pcpsit_key_subquery(b"pcp");
        let proof = db
            .prove_query(&pq, None, v)
            .unwrap()
            .expect("prove pcpsit subquery");
        let (_, results) =
            crate::GroveDb::verify_query(&proof, &pq, v).expect("verify pcpsit subquery");
        assert_eq!(results.len(), 4);
    }

    // -----------------------------------------------------------------
    // Generic-write rejection into a PCPSIT primary (BUG 1 regression)
    // -----------------------------------------------------------------

    #[test]
    fn pcpsit_generic_db_insert_is_rejected_no_partial_write_all_subsets() {
        // Regression: a generic `db.insert` of a leaf directly into a
        // PCPSIT primary must fail closed with an accurate `NotSupported`
        // (the multi-axis generic propagation path has no secondary-mirror
        // hook) and must not partially write. Covered for every axis
        // subset so the multi-axis element shape is exercised too.
        let grove_version = GroveVersion::latest();
        for (i, tags) in all_axis_subsets().iter().enumerate() {
            let db = make_test_grovedb(grove_version);
            let key = format!("pcp_{}", i);
            insert_empty_pcpsit(&db, key.as_bytes(), tags, grove_version);

            let result = db
                .insert(
                    [TEST_LEAF, key.as_bytes()].as_ref(),
                    b"row",
                    Element::new_item_with_sum_item(b"v".to_vec(), 42),
                    None,
                    None,
                    grove_version,
                )
                .unwrap();
            match result {
                Err(Error::NotSupported(msg)) => {
                    assert!(
                        msg.contains("indexed-tree primary")
                            && msg.contains("insert_into_provable_count_provable_sum_indexed_tree"),
                        "expected indexed-primary rejection with dedicated-API pointer for axes \
                         {tags:?}, got: {msg}"
                    );
                }
                other => panic!("expected NotSupported for axes {tags:?}, got {:?}", other),
            }

            // No partial write: primary still empty, leaf absent.
            let parent = db
                .get([TEST_LEAF].as_ref(), key.as_bytes(), None, grove_version)
                .unwrap()
                .expect("get PCPSIT");
            match parent {
                Element::ProvableCountProvableSumIndexedTree(None, 0, 0, _, _) => {}
                other => panic!(
                    "PCPSIT primary must be unchanged after rejected generic insert (axes \
                     {tags:?}), got {:?}",
                    other
                ),
            }
            assert!(db
                .get(
                    [TEST_LEAF, key.as_bytes()].as_ref(),
                    b"row",
                    None,
                    grove_version
                )
                .unwrap()
                .is_err());
            assert_verify_passes(&db, grove_version);
        }
    }

    #[test]
    fn pcpsit_generic_db_insert_after_populated_is_rejected() {
        // A later generic `db.insert` into a populated PCPSIT primary is
        // still refused, leaving the dedicated-API-built state intact.
        let grove_version = GroveVersion::latest();
        let tags = [
            IndexAxis::Count.tag(),
            IndexAxis::Sum.tag(),
            IndexAxis::Avg.tag(),
        ];
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit(&db, b"pcp", &tags, grove_version);
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"pcp"].as_ref(),
            b"row1",
            Element::new_item_with_sum_item(b"v".to_vec(), 42),
            None,
            grove_version,
        )
        .unwrap()
        .expect("dedicated insert");

        let result = db
            .insert(
                [TEST_LEAF, b"pcp"].as_ref(),
                b"row2",
                Element::new_item_with_sum_item(b"w".to_vec(), 8),
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            matches!(result, Err(Error::NotSupported(_))),
            "expected NotSupported, got {:?}",
            result
        );

        let parent = db
            .get([TEST_LEAF].as_ref(), b"pcp", None, grove_version)
            .unwrap()
            .expect("get PCPSIT");
        match parent {
            Element::ProvableCountProvableSumIndexedTree(_, c, s, _, _) => {
                assert_eq!(c, 1);
                assert_eq!(s, 42);
            }
            other => panic!("expected PCPSIT, got {:?}", other),
        }
        assert!(db
            .get([TEST_LEAF, b"pcp"].as_ref(), b"row2", None, grove_version)
            .unwrap()
            .is_err());
        assert_verify_passes(&db, grove_version);
    }

    /// Adding (or removing) an axis on a POPULATED PCPSIT must be rejected.
    ///
    /// `axes_digest` is recomputed from the element's own claimed axes, so a
    /// schema change produced a perfectly consistent-looking element whose
    /// new axis indexed none of the existing rows — `indexed_avg_top_k`
    /// returned an empty set for a populated tree and `verify_grovedb`
    /// reported nothing, because it re-derives the digest from the same
    /// claimed axes. There is no backfill/reindex path.
    #[test]
    fn pcpsit_axes_schema_change_on_a_populated_tree_is_rejected() {
        use grovedb_element::indexed::IndexAxis;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"idx",
            Element::empty_provable_count_provable_sum_indexed_tree(vec![
                (IndexAxis::Count.tag(), None),
                (IndexAxis::Sum.tag(), None),
            ])
            .expect("canonical axes"),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PCPSIT");
        for (k, sum) in [(b"a".as_slice(), 3i64), (b"b".as_slice(), 5)] {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"idx"].as_ref(),
                k,
                Element::new_item_with_sum_item(b"v".to_vec(), sum),
                None,
                grove_version,
            )
            .unwrap()
            .expect("seed entry");
        }

        let stored = db
            .get([TEST_LEAF].as_ref(), b"idx", None, grove_version)
            .unwrap()
            .expect("get PCPSIT");
        let (primary, count, sum, axes) = match stored {
            Element::ProvableCountProvableSumIndexedTree(p, c, s, axes, _) => (p, c, s, axes),
            other => panic!("expected PCPSIT, got {other:?}"),
        };

        // Same element, but with the Avg axis appended.
        let mut widened = axes.clone();
        widened.push((IndexAxis::Avg.tag(), None));
        let override_opts = Some(crate::operations::insert::InsertOptions {
            validate_insertion_does_not_override: false,
            validate_insertion_does_not_override_tree: false,
            base_root_storage_is_free: true,
        });
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"idx",
                Element::ProvableCountProvableSumIndexedTree(
                    primary.clone(),
                    count,
                    sum,
                    widened,
                    None,
                ),
                override_opts.clone(),
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            matches!(result, Err(Error::InvalidInput(m)) if m.contains("axes schema")),
            "widening the axes schema must be rejected, got {result:?}"
        );

        // Re-inserting the element unchanged must still be accepted.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"idx",
            Element::ProvableCountProvableSumIndexedTree(primary, count, sum, axes, None),
            override_opts,
            None,
            grove_version,
        )
        .unwrap()
        .expect("an unchanged re-insert must still be accepted");
    }
}
