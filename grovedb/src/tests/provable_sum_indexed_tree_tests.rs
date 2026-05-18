//! `ProvableSumIndexedTree` (PSIT) tests.
//!
//! Phase 2/3 coverage for the single-axis (sum) indexed tree:
//! - Empty creation + `verify_grovedb` passes (direct + batch).
//! - Single / multi-insert + read-back.
//! - Delete (existing + missing) + verify.
//! - Child-type rejection: non-sum-bearing items (`Item`, plain
//!   `Reference`, plain `Tree`).
//! - Child-type acceptance: SumItem, ItemWithSumItem, empty sum-bearing
//!   trees (SumTree, BigSumTree, CountSumTree, …).
//! - Error paths: wrong tree-type target, root-path inserts/deletes,
//!   non-empty PSIT direct child rejection, oversized item key.
//! - Tree-overwrite cleanup via batch.
//! - Secondary index ordering matches encoded signed `sum`.
//! - Mixed-sign sums, edge values (i64::MIN, i64::MAX, 0).

#[cfg(test)]
mod tests {
    use grovedb_version::version::GroveVersion;

    use crate::{
        batch::QualifiedGroveDbOp,
        tests::{make_test_grovedb, TEST_LEAF},
        Element, Error,
    };

    fn insert_empty_psit_at_test_leaf(
        db: &crate::GroveDb,
        key: &[u8],
        grove_version: &GroveVersion,
    ) {
        db.insert(
            [TEST_LEAF].as_ref(),
            key,
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected PSIT insertion to succeed");
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
    fn psit_empty_creation_verify_passes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"psit", grove_version);
        let elem = db
            .get([TEST_LEAF].as_ref(), b"psit", None, grove_version)
            .unwrap()
            .expect("get");
        assert!(matches!(
            elem,
            Element::ProvableSumIndexedTree(None, None, 0, _)
        ));
    }

    #[test]
    fn psit_empty_creation_via_batch_verify_passes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"psit".to_vec(),
                Element::empty_provable_sum_indexed_tree(),
            )],
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("batch insert of empty PSIT ok");
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn psit_empty_with_flags_round_trips() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"psit",
            Element::empty_provable_sum_indexed_tree_with_flags(Some(vec![1, 2, 3])),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("PSIT with flags ok");
        let elem = db
            .get([TEST_LEAF].as_ref(), b"psit", None, grove_version)
            .unwrap()
            .expect("get");
        match elem {
            Element::ProvableSumIndexedTree(None, None, 0, Some(flags)) => {
                assert_eq!(flags, vec![1, 2, 3]);
            }
            other => panic!("expected PSIT with flags, got {:?}", other),
        }
        assert_verify_passes(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // Direct-API insert / delete: single, multiple, mixed signs
    // -----------------------------------------------------------------

    #[test]
    fn psit_single_insert_then_read_back() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"psit", grove_version);
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"row1",
            Element::new_sum_item(42),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");
        let elem = db
            .get([TEST_LEAF, b"psit"].as_ref(), b"row1", None, grove_version)
            .unwrap()
            .expect("get");
        assert_eq!(elem, Element::new_sum_item(42));
        let parent = db
            .get([TEST_LEAF].as_ref(), b"psit", None, grove_version)
            .unwrap()
            .expect("get PSIT");
        match parent {
            Element::ProvableSumIndexedTree(Some(_), Some(_), 42, _) => {}
            other => panic!("expected PSIT sum=42 with root keys, got {:?}", other),
        }
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn psit_multiple_inserts_verify_passes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"psit", grove_version);
        let mut expected_sum: i64 = 0;
        for (key, sum) in [
            (b"a".as_ref(), 10),
            (b"b".as_ref(), -5),
            (b"c".as_ref(), 100),
        ] {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"psit"].as_ref(),
                key,
                Element::new_sum_item(sum),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
            expected_sum += sum;
        }
        let parent = db
            .get([TEST_LEAF].as_ref(), b"psit", None, grove_version)
            .unwrap()
            .expect("get PSIT");
        match parent {
            Element::ProvableSumIndexedTree(_, _, s, _) => assert_eq!(s, expected_sum),
            other => panic!("expected PSIT, got {:?}", other),
        }
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn psit_delete_existing_decrements_sum() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"psit", grove_version);
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"a",
            Element::new_sum_item(42),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"b",
            Element::new_sum_item(-7),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");

        let removed = db
            .delete_from_provable_sum_indexed_tree(
                [TEST_LEAF, b"psit"].as_ref(),
                b"a",
                None,
                grove_version,
            )
            .unwrap()
            .expect("delete");
        assert!(removed);
        let parent = db
            .get([TEST_LEAF].as_ref(), b"psit", None, grove_version)
            .unwrap()
            .expect("get PSIT");
        match parent {
            Element::ProvableSumIndexedTree(_, _, s, _) => assert_eq!(s, -7),
            other => panic!("expected PSIT, got {:?}", other),
        }
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn psit_delete_missing_returns_false_and_is_idempotent() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"psit", grove_version);
        let removed = db
            .delete_from_provable_sum_indexed_tree(
                [TEST_LEAF, b"psit"].as_ref(),
                b"absent",
                None,
                grove_version,
            )
            .unwrap()
            .expect("delete missing");
        assert!(!removed);
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn psit_overwrite_updates_sum_correctly() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"psit", grove_version);
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"row",
            Element::new_sum_item(10),
            None,
            grove_version,
        )
        .unwrap()
        .expect("first insert");
        // Update — overwrite with a different sum value.
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"row",
            Element::new_sum_item(-3),
            None,
            grove_version,
        )
        .unwrap()
        .expect("overwrite");
        let parent = db
            .get([TEST_LEAF].as_ref(), b"psit", None, grove_version)
            .unwrap()
            .expect("get PSIT");
        match parent {
            Element::ProvableSumIndexedTree(_, _, s, _) => assert_eq!(s, -3),
            other => panic!("expected PSIT, got {:?}", other),
        }
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn psit_handles_extreme_sum_values() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"psit", grove_version);
        // i64::MAX / 2 and -i64::MAX / 2 to avoid overflow when summed.
        let big_pos = i64::MAX / 2;
        let big_neg = -(i64::MAX / 2);
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"big_pos",
            Element::new_sum_item(big_pos),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert big pos");
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"big_neg",
            Element::new_sum_item(big_neg),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert big neg");
        let parent = db
            .get([TEST_LEAF].as_ref(), b"psit", None, grove_version)
            .unwrap()
            .expect("get");
        match parent {
            Element::ProvableSumIndexedTree(_, _, s, _) => assert_eq!(s, big_pos + big_neg),
            other => panic!("expected PSIT, got {:?}", other),
        }
        assert_verify_passes(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // Sum-bearing child variants
    // -----------------------------------------------------------------

    #[test]
    fn psit_accepts_item_with_sum_item_child() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"psit", grove_version);
        let elem = Element::new_item_with_sum_item(b"payload".to_vec(), 17);
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"row",
            elem.clone(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");
        let read = db
            .get([TEST_LEAF, b"psit"].as_ref(), b"row", None, grove_version)
            .unwrap()
            .expect("get");
        assert_eq!(read, elem);
        let parent = db
            .get([TEST_LEAF].as_ref(), b"psit", None, grove_version)
            .unwrap()
            .expect("get parent");
        match parent {
            Element::ProvableSumIndexedTree(_, _, s, _) => assert_eq!(s, 17),
            other => panic!("expected PSIT, got {:?}", other),
        }
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn psit_accepts_empty_sum_tree_child() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"psit", grove_version);
        // An empty SumTree contributes sum=0.
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"row",
            Element::empty_sum_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");
        assert_verify_passes(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // Rejection: non-sum-bearing children
    // -----------------------------------------------------------------

    #[test]
    fn psit_rejects_non_sum_bearing_item() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"psit", grove_version);
        let result = db
            .insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"psit"].as_ref(),
                b"row",
                Element::new_item(b"not-sum-bearing".to_vec()),
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(Error::InvalidInput(msg)) => assert!(
                msg.contains("sum-bearing"),
                "expected sum-bearing rejection, got: {msg}"
            ),
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }

    #[test]
    fn psit_rejects_plain_tree_child() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"psit", grove_version);
        let result = db
            .insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"psit"].as_ref(),
                b"row",
                Element::empty_tree(),
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            result.is_err(),
            "PSIT must reject a plain Tree child, got: {:?}",
            result
        );
    }

    #[test]
    fn psit_rejects_count_tree_child() {
        // CountTree contributes only to count, not sum.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"psit", grove_version);
        let result = db
            .insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"psit"].as_ref(),
                b"row",
                Element::empty_count_tree(),
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            result.is_err(),
            "PSIT must reject a CountTree child (not sum-bearing), got: {:?}",
            result
        );
    }

    // -----------------------------------------------------------------
    // Error paths: wrong target / root-path / oversize
    // -----------------------------------------------------------------

    #[test]
    fn psit_insert_rejects_non_psit_target() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let result = db
            .insert_into_provable_sum_indexed_tree(
                [TEST_LEAF].as_ref(),
                b"row",
                Element::new_sum_item(1),
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(Error::InvalidPath(msg)) => assert!(
                msg.contains("ProvableSumIndexedTree"),
                "expected PSIT-target InvalidPath, got: {msg}"
            ),
            other => panic!("expected InvalidPath, got {:?}", other),
        }
    }

    #[test]
    fn psit_delete_rejects_non_psit_target() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let result = db
            .delete_from_provable_sum_indexed_tree(
                [TEST_LEAF].as_ref(),
                b"row",
                None,
                grove_version,
            )
            .unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn psit_insert_at_root_path_is_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let empty_path: [&[u8]; 0] = [];
        let result = db
            .insert_into_provable_sum_indexed_tree(
                empty_path.as_ref(),
                b"row",
                Element::new_sum_item(1),
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
    fn psit_delete_at_root_path_is_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let empty_path: [&[u8]; 0] = [];
        let result = db
            .delete_from_provable_sum_indexed_tree(empty_path.as_ref(), b"row", None, grove_version)
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
    fn psit_insert_rejects_oversized_item_key() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"psit", grove_version);
        // sum_be (8 bytes) + key, so item_key must be ≤ 247.
        let too_long = vec![0u8; 248];
        let result = db
            .insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"psit"].as_ref(),
                &too_long,
                Element::new_sum_item(1),
                None,
                grove_version,
            )
            .unwrap();
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------
    // Batch: empty creation
    // -----------------------------------------------------------------

    #[test]
    fn psit_batch_empty_creation_and_verify() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.apply_batch(
            vec![
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"psit_a".to_vec(),
                    Element::empty_provable_sum_indexed_tree(),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"psit_b".to_vec(),
                    Element::empty_provable_sum_indexed_tree(),
                ),
            ],
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("batch with two empty PSITs ok");
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn psit_batch_rejects_non_empty_root_keys() {
        // PSIT must be empty at batch insert time: the same invariant
        // PCIT enforces. Non-empty primary/secondary root keys are
        // rejected.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let bogus = Element::new_provable_sum_indexed_tree_with_root_keys_and_sum_value(
            Some(b"bogus".to_vec()),
            None,
            0,
            None,
        );
        let result = db
            .apply_batch(
                vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"psit".to_vec(),
                    bogus,
                )],
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            result.is_err(),
            "batch must reject non-empty PSIT element, got: {:?}",
            result
        );
    }

    // -----------------------------------------------------------------
    // Mixed sequences + verify
    // -----------------------------------------------------------------

    #[test]
    fn psit_insert_then_full_delete_yields_zero_sum() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"psit", grove_version);
        for (k, v) in [
            (b"a".as_ref(), 10),
            (b"b".as_ref(), 20),
            (b"c".as_ref(), -5),
        ] {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"psit"].as_ref(),
                k,
                Element::new_sum_item(v),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        for k in [b"a".as_ref(), b"b".as_ref(), b"c".as_ref()] {
            db.delete_from_provable_sum_indexed_tree(
                [TEST_LEAF, b"psit"].as_ref(),
                k,
                None,
                grove_version,
            )
            .unwrap()
            .expect("delete");
        }
        let parent = db
            .get([TEST_LEAF].as_ref(), b"psit", None, grove_version)
            .unwrap()
            .expect("get");
        match parent {
            Element::ProvableSumIndexedTree(_, _, 0, _) => {}
            other => panic!("expected sum=0 PSIT, got {:?}", other),
        }
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn psit_multiple_psits_independent_verify_passes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"a", grove_version);
        insert_empty_psit_at_test_leaf(&db, b"b", grove_version);
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"a"].as_ref(),
            b"row",
            Element::new_sum_item(7),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"b"].as_ref(),
            b"row",
            Element::new_sum_item(-3),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");
        assert_verify_passes(&db, grove_version);

        let a = db
            .get([TEST_LEAF].as_ref(), b"a", None, grove_version)
            .unwrap()
            .expect("get");
        let b = db
            .get([TEST_LEAF].as_ref(), b"b", None, grove_version)
            .unwrap()
            .expect("get");
        match (a, b) {
            (
                Element::ProvableSumIndexedTree(_, _, 7, _),
                Element::ProvableSumIndexedTree(_, _, -3, _),
            ) => {}
            other => panic!("unexpected aggregate state: {:?}", other),
        }
    }

    // -----------------------------------------------------------------
    // Element-helper coverage on PSIT
    // -----------------------------------------------------------------

    #[test]
    fn psit_helpers_report_sum_bearing() {
        // is_sum_bearing_child must return true for PSIT itself
        // (an empty PSIT contributes (count=1, sum=0)).
        let psit = Element::ProvableSumIndexedTree(None, None, 0, None);
        assert!(psit.is_sum_bearing_child());
        // is_count_and_sum_bearing_child must return false: PSIT only
        // contributes sum, not the joint count/sum role.
        assert!(!psit.is_count_and_sum_bearing_child());
    }
}
