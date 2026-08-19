//! `ProvableSumIndexedTree` (PSIT) tests.
//!
//! Phase 2/3 coverage for the single-axis (sum) indexed tree:
//! - Empty creation + `verify_grovedb` passes (direct + batch).
//! - Single / multi-insert + read-back.
//! - Delete (existing + missing) + verify.
//! - Child-type rejection: non-sum-bearing items (`Item`, plain
//!   `Reference`, plain `Tree`).
//! - Child-type acceptance: SumItem, ItemWithSumItem, and empty i64
//!   sum-bearing trees (SumTree, CountSumTree, …); BigSumTree is rejected.
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

    // -----------------------------------------------------------------
    // Direct-API db.insert: on-disk validated path with non-empty
    // root keys
    // -----------------------------------------------------------------

    #[test]
    fn psit_direct_insert_rejects_partial_root_keys() {
        // The direct `db.insert` path for PSIT requires that BOTH
        // primary_root_key and secondary_root_key are set when the
        // PSIT is non-empty (sum != 0 OR either key is Some(_)).
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let bad = Element::new_provable_sum_indexed_tree_with_root_keys_and_sum_value(
            Some(b"primary".to_vec()),
            None,
            10,
            None,
        );
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"psit",
                bad,
                None,
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(Error::InvalidInput(msg)) => assert!(
                msg.contains("BOTH") || msg.contains("partial state"),
                "expected partial-state rejection, got: {msg}"
            ),
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }

    #[test]
    fn psit_direct_insert_rejects_root_key_mismatch() {
        // If the PSIT claims a primary_root_key that doesn't match the
        // existing primary Merk's root_key, the direct insert path
        // must reject — preventing forged claims via the regular
        // db.insert API.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"psit", grove_version);
        // Try to overwrite with a non-empty claim whose primary key
        // and secondary key are bogus.
        let bad = Element::new_provable_sum_indexed_tree_with_root_keys_and_sum_value(
            Some(b"bogus_primary".to_vec()),
            Some(b"bogus_secondary".to_vec()),
            42,
            None,
        );
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"psit",
                bad,
                None,
                None,
                grove_version,
            )
            .unwrap();
        // Even tree-override is on by default so this fails. The
        // important thing is the operation does not succeed and
        // therefore doesn't corrupt the state.
        assert!(
            result.is_err(),
            "PSIT with bogus root keys must not succeed, got: {:?}",
            result
        );
    }

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

    // -----------------------------------------------------------------
    // Phase 3: indexed_sum_* direct query APIs over PSIT
    // -----------------------------------------------------------------

    /// Populate the PSIT under `[TEST_LEAF, "psit"]` with a known set
    /// of `(key, sum)` pairs spanning negative / zero / positive.
    fn psit_populate_known_set(db: &crate::GroveDb, grove_version: &GroveVersion) {
        for (k, s) in [
            (b"alice".as_ref(), 5i64),
            (b"bob", 12),
            (b"carol", -7),
            (b"dave", 0),
            (b"eve", -1),
            (b"frank", 100),
        ] {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"psit"].as_ref(),
                k,
                Element::new_sum_item(s),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert sum item");
        }
    }

    #[test]
    fn indexed_sum_top_k_on_psit_descending_returns_highest_first() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"psit", grove_version);
        psit_populate_known_set(&db, grove_version);

        // Sorted ascending by sum: carol(-7), eve(-1), dave(0), alice(5),
        // bob(12), frank(100).

        // Top 3 descending: frank(100), bob(12), alice(5).
        let top3 = db
            .indexed_sum_top_k([TEST_LEAF, b"psit"].as_ref(), 3, true, None, grove_version)
            .unwrap()
            .expect("top-k desc");
        assert_axis_entries_eq!(
            top3,
            vec![
                (100i64, b"frank".to_vec()),
                (12, b"bob".to_vec()),
                (5, b"alice".to_vec()),
            ]
        );

        // Top 2 ascending: carol(-7), eve(-1).
        let bottom2 = db
            .indexed_sum_top_k([TEST_LEAF, b"psit"].as_ref(), 2, false, None, grove_version)
            .unwrap()
            .expect("bottom-2");
        assert_axis_entries_eq!(
            bottom2,
            vec![(-7i64, b"carol".to_vec()), (-1, b"eve".to_vec())]
        );
    }

    #[test]
    fn indexed_sum_top_k_on_empty_psit_returns_empty() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"psit", grove_version);
        let top = db
            .indexed_sum_top_k([TEST_LEAF, b"psit"].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("empty");
        assert!(top.is_empty());
    }

    #[test]
    fn indexed_sum_top_k_negative_lt_zero_lt_positive_ordering() {
        // The sign-flipped sum-sort-key encoding must put negatives
        // before zero and zero before positives in ascending lex order.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"psit", grove_version);

        // Three entries: one negative, one zero, one positive.
        for (k, s) in [(b"neg".as_ref(), -42i64), (b"zero", 0), (b"pos", 17)] {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"psit"].as_ref(),
                k,
                Element::new_sum_item(s),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }

        let asc = db
            .indexed_sum_top_k(
                [TEST_LEAF, b"psit"].as_ref(),
                10,
                false,
                None,
                grove_version,
            )
            .unwrap()
            .expect("asc");
        assert_axis_entries_eq!(
            asc,
            vec![
                (-42i64, b"neg".to_vec()),
                (0, b"zero".to_vec()),
                (17, b"pos".to_vec()),
            ]
        );
    }

    #[test]
    fn indexed_sum_top_k_paginated_pages_through_dataset() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"psit", grove_version);
        psit_populate_known_set(&db, grove_version);

        // Descending full scan: frank(100), bob(12), alice(5),
        // dave(0), eve(-1), carol(-7).
        let page1 = db
            .indexed_sum_top_k_paginated(
                [TEST_LEAF, b"psit"].as_ref(),
                2,
                0,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("page 1");
        assert_axis_entries_eq!(
            page1.entries,
            vec![(100i64, b"frank".to_vec()), (12, b"bob".to_vec())]
        );

        let page2 = db
            .indexed_sum_top_k_paginated(
                [TEST_LEAF, b"psit"].as_ref(),
                2,
                2,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("page 2");
        assert_axis_entries_eq!(
            page2.entries,
            vec![(5i64, b"alice".to_vec()), (0, b"dave".to_vec())]
        );

        // Offset beyond end → empty.
        let beyond = db
            .indexed_sum_top_k_paginated(
                [TEST_LEAF, b"psit"].as_ref(),
                5,
                100,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("beyond");
        assert!(beyond.entries.is_empty());

        // offset=0 ≡ plain top_k.
        let plain = db
            .indexed_sum_top_k([TEST_LEAF, b"psit"].as_ref(), 3, true, None, grove_version)
            .unwrap()
            .expect("plain");
        let pag = db
            .indexed_sum_top_k_paginated(
                [TEST_LEAF, b"psit"].as_ref(),
                3,
                0,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("pag offset 0");
        assert_eq!(plain, pag.entries);
    }

    #[test]
    fn indexed_sum_range_inclusive_bounds_match_plain_filter() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"psit", grove_version);
        psit_populate_known_set(&db, grove_version);

        // Range [-1, 12] ascending: eve(-1), dave(0), alice(5), bob(12).
        let in_range = db
            .indexed_sum_range(
                [TEST_LEAF, b"psit"].as_ref(),
                -1,
                12,
                false,
                100,
                None,
                grove_version,
            )
            .unwrap()
            .expect("range");
        assert_axis_entries_eq!(
            in_range,
            vec![
                (-1i64, b"eve".to_vec()),
                (0, b"dave".to_vec()),
                (5, b"alice".to_vec()),
                (12, b"bob".to_vec()),
            ]
        );

        // Descending same range.
        let desc = db
            .indexed_sum_range(
                [TEST_LEAF, b"psit"].as_ref(),
                -1,
                12,
                true,
                100,
                None,
                grove_version,
            )
            .unwrap()
            .expect("desc");
        assert_axis_entries_eq!(
            desc,
            vec![
                (12i64, b"bob".to_vec()),
                (5, b"alice".to_vec()),
                (0, b"dave".to_vec()),
                (-1, b"eve".to_vec()),
            ]
        );

        // Exact match: [12, 12] → bob.
        let exact = db
            .indexed_sum_range(
                [TEST_LEAF, b"psit"].as_ref(),
                12,
                12,
                false,
                100,
                None,
                grove_version,
            )
            .unwrap()
            .expect("exact");
        assert_axis_entries_eq!(exact, vec![(12i64, b"bob".to_vec())]);

        // lo > hi: empty.
        let empty = db
            .indexed_sum_range(
                [TEST_LEAF, b"psit"].as_ref(),
                100,
                10,
                false,
                100,
                None,
                grove_version,
            )
            .unwrap()
            .expect("lo>hi");
        assert!(empty.is_empty());

        // Full scan [i64::MIN, i64::MAX].
        let full = db
            .indexed_sum_range(
                [TEST_LEAF, b"psit"].as_ref(),
                i64::MIN,
                i64::MAX,
                false,
                100,
                None,
                grove_version,
            )
            .unwrap()
            .expect("full");
        assert_eq!(full.len(), 6);

        // limit=0 returns empty.
        let zero_limit = db
            .indexed_sum_range(
                [TEST_LEAF, b"psit"].as_ref(),
                i64::MIN,
                i64::MAX,
                false,
                0,
                None,
                grove_version,
            )
            .unwrap()
            .expect("limit 0");
        assert!(zero_limit.is_empty());
    }

    #[test]
    fn indexed_sum_range_negative_bounds() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"psit", grove_version);
        psit_populate_known_set(&db, grove_version);

        // Negative-only range [-7, -1]: carol(-7), eve(-1).
        let neg = db
            .indexed_sum_range(
                [TEST_LEAF, b"psit"].as_ref(),
                -7,
                -1,
                false,
                100,
                None,
                grove_version,
            )
            .unwrap()
            .expect("neg range");
        assert_axis_entries_eq!(neg, vec![(-7i64, b"carol".to_vec()), (-1, b"eve".to_vec())]);
    }

    #[test]
    fn indexed_sum_aggregate_over_value_range_sums_in_range() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"psit", grove_version);
        psit_populate_known_set(&db, grove_version);

        // Sum in [-1, 12]: -1 + 0 + 5 + 12 = 16.
        let agg = db
            .indexed_sum_aggregate_over_value_range(
                [TEST_LEAF, b"psit"].as_ref(),
                -1,
                12,
                None,
                grove_version,
            )
            .unwrap()
            .expect("agg");
        assert_eq!(agg, 16);

        // Total sum [i64::MIN, i64::MAX]: -7 + -1 + 0 + 5 + 12 + 100 = 109.
        let total = db
            .indexed_sum_aggregate_over_value_range(
                [TEST_LEAF, b"psit"].as_ref(),
                i64::MIN,
                i64::MAX,
                None,
                grove_version,
            )
            .unwrap()
            .expect("total");
        assert_eq!(total, 109);

        // Empty range [200, 300]: 0.
        let none = db
            .indexed_sum_aggregate_over_value_range(
                [TEST_LEAF, b"psit"].as_ref(),
                200,
                300,
                None,
                grove_version,
            )
            .unwrap()
            .expect("none");
        assert_eq!(none, 0);

        // lo > hi: 0.
        let degenerate = db
            .indexed_sum_aggregate_over_value_range(
                [TEST_LEAF, b"psit"].as_ref(),
                100,
                10,
                None,
                grove_version,
            )
            .unwrap()
            .expect("degen");
        assert_eq!(degenerate, 0);

        // Negative-only [-100, 0]: -7 + -1 + 0 = -8.
        let neg = db
            .indexed_sum_aggregate_over_value_range(
                [TEST_LEAF, b"psit"].as_ref(),
                -100,
                0,
                None,
                grove_version,
            )
            .unwrap()
            .expect("neg");
        assert_eq!(neg, -8);
    }

    // ---- axis-compatibility rejection on PSIT ----

    #[test]
    fn indexed_count_top_k_on_psit_returns_invalid_path() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"psit", grove_version);
        let result = db
            .indexed_count_top_k([TEST_LEAF, b"psit"].as_ref(), 5, true, None, grove_version)
            .unwrap();
        match result {
            Err(Error::InvalidPath(msg)) => {
                assert!(
                    msg.contains("Count") && msg.contains("not indexed"),
                    "expected axis-compat rejection, got: {msg}"
                );
            }
            other => panic!("expected InvalidPath, got {:?}", other),
        }
    }

    #[test]
    fn indexed_avg_top_k_on_psit_returns_invalid_path() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"psit", grove_version);
        let result = db
            .indexed_avg_top_k([TEST_LEAF, b"psit"].as_ref(), 5, true, None, grove_version)
            .unwrap();
        match result {
            Err(Error::InvalidPath(msg)) => {
                assert!(
                    msg.contains("Avg") && msg.contains("not indexed"),
                    "expected axis-compat rejection, got: {msg}"
                );
            }
            other => panic!("expected InvalidPath, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------
    // Depth > 1 propagation
    // -----------------------------------------------------------------

    #[test]
    fn psit_depth_2_under_tree_propagates_sum_and_verifies() {
        // PSIT under a regular Tree at depth=2.
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
        db.insert(
            [TEST_LEAF, b"parent"].as_ref(),
            b"psit",
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("psit");
        for (k, v) in [(b"a".as_ref(), 10i64), (b"b", -20), (b"c", 30)] {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"parent", b"psit"].as_ref(),
                k,
                Element::new_sum_item(v),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        let elem = db
            .get(
                [TEST_LEAF, b"parent"].as_ref(),
                b"psit",
                None,
                grove_version,
            )
            .unwrap()
            .expect("get");
        match elem.underlying() {
            Element::ProvableSumIndexedTree(_, _, s, _) => assert_eq!(*s, 20),
            other => panic!("expected PSIT, got {:?}", other),
        }
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn psit_depth_3_propagates_sum_and_verifies() {
        // PSIT under two Trees (depth=3): TEST_LEAF/p1/p2/psit
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"p1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("p1");
        db.insert(
            [TEST_LEAF, b"p1"].as_ref(),
            b"p2",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("p2");
        db.insert(
            [TEST_LEAF, b"p1", b"p2"].as_ref(),
            b"psit",
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("psit");
        for (k, v) in [(b"x".as_ref(), 50i64), (b"y", 30)] {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"p1", b"p2", b"psit"].as_ref(),
                k,
                Element::new_sum_item(v),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        assert_verify_passes(&db, grove_version);
        let elem = db
            .get(
                [TEST_LEAF, b"p1", b"p2"].as_ref(),
                b"psit",
                None,
                grove_version,
            )
            .unwrap()
            .expect("get");
        match elem.underlying() {
            Element::ProvableSumIndexedTree(_, _, s, _) => assert_eq!(*s, 80),
            other => panic!("expected PSIT, got {:?}", other),
        }
    }

    #[test]
    fn psit_delete_then_reinsert_at_depth_2_consistent() {
        // PSIT at depth 2 with delete + re-insert; verify_grovedb still
        // passes and sum is correct.
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
        db.insert(
            [TEST_LEAF, b"parent"].as_ref(),
            b"psit",
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("psit");
        for (k, v) in [(b"a".as_ref(), 10i64), (b"b", 20)] {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"parent", b"psit"].as_ref(),
                k,
                Element::new_sum_item(v),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        db.delete_from_provable_sum_indexed_tree(
            [TEST_LEAF, b"parent", b"psit"].as_ref(),
            b"a",
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete");
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"parent", b"psit"].as_ref(),
            b"c",
            Element::new_sum_item(15),
            None,
            grove_version,
        )
        .unwrap()
        .expect("c");
        assert_verify_passes(&db, grove_version);
        let elem = db
            .get(
                [TEST_LEAF, b"parent"].as_ref(),
                b"psit",
                None,
                grove_version,
            )
            .unwrap()
            .expect("get");
        match elem.underlying() {
            Element::ProvableSumIndexedTree(_, _, s, _) => assert_eq!(*s, 35),
            other => panic!("expected PSIT, got {:?}", other),
        }
    }

    /// Security regression (P1): the dedicated PSIT insert short-circuits
    /// child subtree roots to NULL_HASH, so it must reject a non-empty
    /// `SumTree(Some(root_key), ..)` child claim — otherwise the
    /// serialized element would persist a root_key that disagrees with
    /// the empty merk node it is bound to.
    #[test]
    fn psit_rejects_non_empty_sum_tree_child() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"psit",
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("create PSIT");

        // A SumTree claiming a non-empty root must be rejected.
        let res = db
            .insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"psit"].as_ref(),
                b"k",
                Element::SumTree(Some(vec![7u8; 32]), 0, None),
                None,
                v,
            )
            .unwrap();
        assert!(
            matches!(res, Err(Error::NotSupported(_))),
            "non-empty SumTree child must be rejected by the dedicated PSIT insert guard; \
             got {res:?}"
        );

        // An EMPTY SumTree child is accepted.
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"k",
            Element::SumTree(None, 0, None),
            None,
            v,
        )
        .unwrap()
        .expect("empty SumTree child accepted");
        assert!(db
            .verify_grovedb(None, true, true, v)
            .expect("verify_grovedb after empty SumTree child")
            .is_empty());
    }

    /// Correctness regression (P1): deleting a tree child from a PSIT
    /// primary must run the orphan-cleanup path (find_subtrees + clear,
    /// plus secondary-namespace clear for indexed children) that the PCIT
    /// path has always had. Exercises the `is_layered_target` delete
    /// branch on an empty `SumTree` child and confirms verify_grovedb
    /// stays clean across delete + re-create.
    ///
    /// NOTE: populating a tree child *under* a PSIT primary via the
    /// generic deep-insert path is a known Phase-2 deferral ("can only
    /// propagate on tree items"), so a populated orphan cannot yet be
    /// produced through the public API — the cleanup added here is the
    /// forward-looking mirror of PCIT's behavior for when that nesting
    /// lands. This test guards that the cleanup branch executes cleanly.
    #[test]
    fn psit_delete_tree_child_runs_cleanup_path() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"psit",
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("create PSIT");

        // Insert an empty SumTree child (is_layered_target on delete).
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"child",
            Element::SumTree(None, 0, None),
            None,
            v,
        )
        .unwrap()
        .expect("insert empty SumTree child");
        assert!(db
            .verify_grovedb(None, true, true, v)
            .expect("verify after insert")
            .is_empty());

        // Delete it — exercises the cleanup branch.
        let removed = db
            .delete_from_provable_sum_indexed_tree([TEST_LEAF, b"psit"].as_ref(), b"child", None, v)
            .unwrap()
            .expect("delete child from PSIT");
        assert!(removed);
        assert!(db
            .verify_grovedb(None, true, true, v)
            .expect("verify after delete")
            .is_empty());

        // Re-create an empty SumTree at the same key; verify stays clean.
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"child",
            Element::SumTree(None, 0, None),
            None,
            v,
        )
        .unwrap()
        .expect("re-create empty SumTree child");
        assert!(db
            .verify_grovedb(None, true, true, v)
            .expect("verify after re-create")
            .is_empty());
    }

    /// Direct `db.insert` of a NON-EMPTY PSIT validates the claimed
    /// primary + secondary root keys against on-disk state and succeeds
    /// when they match. Exercises the non-empty success path in
    /// `operations/insert/mod.rs`.
    #[test]
    fn psit_direct_insert_non_empty_with_matching_roots_succeeds() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"psit",
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("create PSIT");
        for (k, s) in [(b"a".as_ref(), 10i64), (b"b", 20)] {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"psit"].as_ref(),
                k,
                Element::new_sum_item(s),
                None,
                v,
            )
            .unwrap()
            .expect("populate");
        }
        let populated = db
            .get([TEST_LEAF].as_ref(), b"psit", None, v)
            .unwrap()
            .expect("get populated PSIT");
        assert!(matches!(
            populated,
            Element::ProvableSumIndexedTree(Some(_), Some(_), 30, _)
        ));
        let opts = crate::operations::insert::InsertOptions {
            validate_insertion_does_not_override: false,
            validate_insertion_does_not_override_tree: false,
            base_root_storage_is_free: true,
        };
        db.insert(
            [TEST_LEAF].as_ref(),
            b"psit",
            populated,
            Some(opts),
            None,
            v,
        )
        .unwrap()
        .expect("re-insert non-empty PSIT with matching roots");
        assert!(db
            .verify_grovedb(None, true, true, v)
            .expect("verify_grovedb")
            .is_empty());
    }

    /// Direct `db.insert` of a non-empty PSIT with a mismatched primary
    /// root key must be rejected.
    #[test]
    fn psit_direct_insert_non_empty_with_mismatched_primary_root_rejected() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"psit",
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("create PSIT");
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"a",
            Element::new_sum_item(10),
            None,
            v,
        )
        .unwrap()
        .expect("populate");
        let populated = db
            .get([TEST_LEAF].as_ref(), b"psit", None, v)
            .unwrap()
            .expect("get");
        let Element::ProvableSumIndexedTree(_primary, sec, sum, flags) = populated else {
            panic!("expected PSIT");
        };
        // Wrong primary root key.
        let tampered = Element::ProvableSumIndexedTree(Some(vec![0xEF; 32]), sec, sum, flags);
        let opts = crate::operations::insert::InsertOptions {
            validate_insertion_does_not_override: false,
            validate_insertion_does_not_override_tree: false,
            base_root_storage_is_free: true,
        };
        let res = db
            .insert([TEST_LEAF].as_ref(), b"psit", tampered, Some(opts), None, v)
            .unwrap();
        assert!(
            matches!(res, Err(Error::InvalidInput(_))),
            "mismatched primary root key must be rejected; got {res:?}"
        );
    }

    // -----------------------------------------------------------------
    // V1 proof regression tests (PR #657 BUG 1 + BUG 2)
    //
    // Before the fix:
    //   - An empty PSIT selected by a V1 proof was rejected with
    //     "V1 empty tree value hash mismatch" because the verifier used
    //     the two-input combine_hash form instead of the three-input
    //     combine_hash_three(H(value), NULL_HASH, NULL_HASH) that the
    //     insert path commits for indexed trees (BUG 2a / 2b).
    //   - A non-empty PSIT crossed by a subquery was silently skipped by
    //     the prover (fell through to `=> continue`) so the honest proof
    //     was missing the lower layer and the verifier rejected it
    //     (BUG 1). The verifier also rejected PSIT descent outright with
    //     a "Phase 2" NotSupported error.
    //
    // These lock the fixed behavior at full parity with PCIT.
    // -----------------------------------------------------------------

    fn key_query(key: &[u8]) -> crate::PathQuery {
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

    /// `RangeFull` over TEST_LEAF with no subquery — the shape a caller uses
    /// to list a directory that happens to contain an indexed tree.
    fn range_full_query() -> crate::PathQuery {
        use crate::{query::SizedQuery, PathQuery, QueryItem, SubqueryBranch};
        PathQuery {
            path: vec![TEST_LEAF.to_vec()],
            query: SizedQuery {
                query: grovedb_merk::proofs::Query {
                    items: vec![QueryItem::RangeFull(..)],
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

    /// A `RangeFull` listing whose result set merely CONTAINS a populated
    /// indexed tree must prove and verify. Before the terminal-attestation
    /// envelope this proved Ok and then failed verification with
    /// "must use KVValueHashFeatureTypeWithChildHash", which made every
    /// generic query over a directory holding an indexed tree unusable.
    #[test]
    fn psit_range_full_containing_populated_psit_verifies() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        insert_empty_psit_at_test_leaf(&db, b"psit", v);
        psit_populate_known_set(&db, v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"plain",
            Element::new_item(b"sibling".to_vec()),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("sibling item");

        let pq = range_full_query();
        let proof = db.prove_query(&pq, None, v).unwrap().expect("prove");
        let (root, items) =
            crate::GroveDb::verify_query(&proof, &pq, v).expect("range listing must verify");
        assert_eq!(
            root,
            db.root_hash(None, v).unwrap().unwrap(),
            "verified root must match the live root"
        );
        assert_eq!(items.len(), 2, "the PSIT element and the sibling item");
    }

    /// The terminal attestation is what binds the indexed element's bytes to
    /// the parent-committed `value_hash`. Substituting forged element bytes
    /// (a `KVValueHash` node commits only to key + value_hash, so the merk
    /// proof itself still checks out) must be caught by the
    /// `combine_hash_three` comparison.
    #[test]
    fn psit_terminal_attestation_rejects_forged_element_bytes() {
        use crate::operations::proof::{GroveDBProof, ProofBytes};

        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        insert_empty_psit_at_test_leaf(&db, b"psit", v);
        psit_populate_known_set(&db, v);

        let pq = key_query(b"psit");
        let proof_bytes = db.prove_query(&pq, None, v).unwrap().expect("prove");
        crate::GroveDb::verify_query(&proof_bytes, &pq, v).expect("honest proof verifies");

        let config = bincode::config::standard();
        let (decoded, _): (GroveDBProof, _) =
            bincode::decode_from_slice(&proof_bytes, config).expect("decode");
        let GroveDBProof::V1(mut v1) = decoded else {
            panic!("expected a V1 proof");
        };

        // The terminal envelope is present for the populated PSIT.
        let terminal = v1
            .root_layer
            .lower_layers
            .values_mut()
            .find_map(|layer| {
                layer
                    .lower_layers
                    .values_mut()
                    .find(|l| matches!(l.merk_proof, ProofBytes::IndexedTreeTerminal(_)))
            })
            .expect("populated PSIT must carry a terminal attestation");
        // Flip one byte of the attested primary root: the element bytes now
        // no longer combine to the committed value hash.
        if let ProofBytes::IndexedTreeTerminal(bytes) = &mut terminal.merk_proof {
            bytes[32] ^= 0xff;
        }

        let tampered = bincode::encode_to_vec(GroveDBProof::V1(v1), config).expect("encode");
        let result = crate::GroveDb::verify_query(&tampered, &pq, v);
        assert!(
            matches!(result, Err(crate::Error::InvalidProof(_, ref m))
                if m.contains("indexed terminal attestation")),
            "forged attestation must be rejected, got {result:?}"
        );
    }

    fn key_subquery(key: &[u8]) -> crate::PathQuery {
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
    fn psit_empty_v1_proof_terminal_verifies() {
        // BUG 2: empty PSIT selected as a terminal V1 result must verify
        // against combine_hash_three(H(value), NULL_HASH, NULL_HASH).
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        insert_empty_psit_at_test_leaf(&db, b"psit", v);
        let pq = key_query(b"psit");
        let proof = db
            .prove_query(&pq, None, v)
            .unwrap()
            .expect("prove empty psit");
        let (root, results) =
            crate::GroveDb::verify_query(&proof, &pq, v).expect("verify empty psit terminal");
        assert_eq!(root, db.root_hash(None, v).unwrap().expect("root"));
        assert_eq!(
            results.len(),
            1,
            "empty PSIT should be a single terminal result"
        );
    }

    #[test]
    fn psit_non_empty_v1_subquery_verifies_and_matches_pcit() {
        // BUG 1: a non-empty PSIT crossed by a subquery must descend into
        // the primary and verify via combine_hash_three(H(value),
        // primary_root, secondary_root) — the same envelope PCIT uses.
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        insert_empty_psit_at_test_leaf(&db, b"psit", v);
        psit_populate_known_set(&db, v); // 6 rows

        let pq = key_subquery(b"psit");
        let proof = db
            .prove_query(&pq, None, v)
            .unwrap()
            .expect("prove psit subquery");
        let (root, results) =
            crate::GroveDb::verify_query(&proof, &pq, v).expect("verify psit subquery");
        assert_eq!(root, db.root_hash(None, v).unwrap().expect("root"));
        assert_eq!(results.len(), 6, "subquery must return all six PSIT rows");

        // Parity check: the equivalent PCIT query returns the same shape.
        let db2 = make_test_grovedb(v);
        db2.insert(
            [TEST_LEAF].as_ref(),
            b"pcit",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("pcit");
        for k in [b"a".as_ref(), b"b", b"c", b"d", b"e", b"f"] {
            db2.insert_into_count_indexed_tree(
                [TEST_LEAF, b"pcit"].as_ref(),
                k,
                Element::new_item(b"v".to_vec()),
                None,
                v,
            )
            .unwrap()
            .expect("pop pcit");
        }
        let pq2 = key_subquery(b"pcit");
        let proof2 = db2
            .prove_query(&pq2, None, v)
            .unwrap()
            .expect("prove pcit subquery");
        let (_, results2) =
            crate::GroveDb::verify_query(&proof2, &pq2, v).expect("verify pcit subquery");
        assert_eq!(
            results.len(),
            results2.len(),
            "PSIT subquery result count must match PCIT"
        );
    }

    #[test]
    fn psit_non_empty_v1_direct_key_verifies_like_pcit() {
        // A non-empty indexed tree selected by a direct key with NO subquery
        // must prove AND verify, identically for PSIT and PCIT.
        //
        // This previously asserted the opposite: the prover emitted an
        // unbound node (the `KVValueHashFeatureTypeWithChildHash` upgrade
        // used for regular trees cannot express the three-input indexed
        // binding), the verifier demanded that node type, and every honest
        // proof containing a populated indexed tree was rejected. The
        // prover now emits a `ProofBytes::IndexedTreeTerminal` attestation
        // (secondary attestation ‖ primary root) and the verifier checks
        // `combine_hash_three` against it.
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        insert_empty_psit_at_test_leaf(&db, b"psit", v);
        psit_populate_known_set(&db, v);
        let pq = key_query(b"psit");
        let proof = db.prove_query(&pq, None, v).unwrap().expect("prove");
        let psit_res = crate::GroveDb::verify_query(&proof, &pq, v);

        let db2 = make_test_grovedb(v);
        db2.insert(
            [TEST_LEAF].as_ref(),
            b"pcit",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("pcit");
        db2.insert_into_count_indexed_tree(
            [TEST_LEAF, b"pcit"].as_ref(),
            b"a",
            Element::new_item(b"v".to_vec()),
            None,
            v,
        )
        .unwrap()
        .expect("pop pcit");
        let pq2 = key_query(b"pcit");
        let proof2 = db2.prove_query(&pq2, None, v).unwrap().expect("prove pcit");
        let pcit_res = crate::GroveDb::verify_query(&proof2, &pq2, v);

        assert_eq!(
            psit_res.is_err(),
            pcit_res.is_err(),
            "PSIT and PCIT direct-key non-empty verification must agree"
        );
        let (_, psit_items) = psit_res.expect("non-empty direct-key PSIT proof must verify");
        let (_, pcit_items) = pcit_res.expect("non-empty direct-key PCIT proof must verify");
        assert_eq!(
            psit_items.len(),
            1,
            "the PSIT element itself is the single result"
        );
        assert_eq!(
            pcit_items.len(),
            1,
            "the PCIT element itself is the single result"
        );
    }

    // -----------------------------------------------------------------
    // Generic-write rejection into a PSIT primary (BUG 1 regression)
    // -----------------------------------------------------------------

    #[test]
    fn psit_generic_db_insert_of_sum_item_is_rejected_no_partial_write() {
        // Regression for the bug where a generic `db.insert` of a leaf
        // (sum) item directly into a PSIT primary failed with a
        // misleading `InvalidPath("can only propagate on tree items")`.
        // The generic path has no secondary-mirror hook, so it must fail
        // closed with an accurate `NotSupported` pointing at the dedicated
        // API — and must NOT partially write (the batch is discarded on
        // the propagation error before commit).
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"psit", grove_version);

        let result = db
            .insert(
                [TEST_LEAF, b"psit"].as_ref(),
                b"a",
                Element::new_sum_item(5),
                None,
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(Error::NotSupported(msg)) => {
                assert!(
                    msg.contains("indexed-tree primary")
                        && msg.contains("insert_into_provable_sum_indexed_tree"),
                    "expected indexed-primary rejection with dedicated-API pointer, got: {msg}"
                );
            }
            other => panic!("expected NotSupported, got {:?}", other),
        }

        // No partial write: the PSIT is still empty and the leaf is absent.
        let elem = db
            .get([TEST_LEAF].as_ref(), b"psit", None, grove_version)
            .unwrap()
            .expect("get PSIT");
        assert!(
            matches!(elem, Element::ProvableSumIndexedTree(None, None, 0, _)),
            "PSIT primary must be unchanged after rejected generic insert, got {:?}",
            elem
        );
        let leaf = db
            .get([TEST_LEAF, b"psit"].as_ref(), b"a", None, grove_version)
            .unwrap();
        assert!(
            matches!(leaf, Err(Error::PathKeyNotFound(_))),
            "leaf must not have been written, got {:?}",
            leaf
        );

        // verify_grovedb stays clean after the rejected insert.
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn psit_generic_db_insert_after_populated_is_rejected() {
        // Same rejection holds when the PSIT already has entries (inserted
        // via the dedicated API): a later generic `db.insert` into the
        // primary is still refused, and the existing (correct) state is
        // untouched.
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
        .expect("dedicated insert");

        let result = db
            .insert(
                [TEST_LEAF, b"psit"].as_ref(),
                b"row2",
                Element::new_sum_item(8),
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

        // Existing state unchanged: sum still 42, row2 absent.
        let parent = db
            .get([TEST_LEAF].as_ref(), b"psit", None, grove_version)
            .unwrap()
            .expect("get PSIT");
        match parent {
            Element::ProvableSumIndexedTree(_, _, s, _) => assert_eq!(s, 42),
            other => panic!("expected PSIT, got {:?}", other),
        }
        assert!(db
            .get([TEST_LEAF, b"psit"].as_ref(), b"row2", None, grove_version)
            .unwrap()
            .is_err());
        assert_verify_passes(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // Writes BELOW a child of a PSIT / PCPSIT primary
    // -----------------------------------------------------------------

    /// A sum-bearing tree child is explicitly accepted into a PSIT primary,
    /// so writing into that child must work. Change propagation used to gate its
    /// indexed handling on `is_count_indexed_primary()` (PCIT only), so the
    /// PSIT element fell through to `update_tree_item_preserve_flag` ->
    /// `reconstruct_with_root_key`, which has no indexed arm, and every such
    /// write died with `InvalidPath("can only propagate on tree items")` —
    /// making an accepted child shape a permanent dead end.
    #[test]
    fn writes_below_a_tree_child_of_a_psit_propagate() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        insert_empty_psit_at_test_leaf(&db, b"psit", v);
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"child",
            Element::empty_sum_tree(),
            None,
            v,
        )
        .unwrap()
        .expect("sum-tree child under PSIT");

        db.insert(
            [TEST_LEAF, b"psit", b"child"].as_ref(),
            b"k",
            Element::new_sum_item(5),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("writing below a PSIT child must propagate");

        let fetched = db
            .get([TEST_LEAF, b"psit", b"child"].as_ref(), b"k", None, v)
            .unwrap()
            .expect("value round-trips");
        assert_eq!(fetched, Element::new_sum_item(5));

        let issues = db.verify_grovedb(None, true, false, v).unwrap();
        assert!(issues.is_empty(), "verify_grovedb reported: {issues:?}");
    }

    /// Same for PCPSIT, whose element rebuilds through `reconstruct_with_axes`
    /// and re-derives the axes digest over every configured axis.
    #[test]
    fn writes_below_a_tree_child_of_a_pcpsit_propagate() {
        use grovedb_element::indexed::IndexAxis;

        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            Element::empty_provable_count_provable_sum_indexed_tree(vec![
                (IndexAxis::Count.tag(), None),
                (IndexAxis::Sum.tag(), None),
            ])
            .expect("canonical axes"),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("create PCPSIT");
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"pcpsit"].as_ref(),
            b"child",
            Element::empty_count_sum_tree(),
            None,
            v,
        )
        .unwrap()
        .expect("count-sum-tree child under PCPSIT");

        db.insert(
            [TEST_LEAF, b"pcpsit", b"child"].as_ref(),
            b"k",
            Element::new_item_with_sum_item(b"v".to_vec(), 7),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("writing below a PCPSIT child must propagate");

        let issues = db.verify_grovedb(None, true, false, v).unwrap();
        assert!(issues.is_empty(), "verify_grovedb reported: {issues:?}");
    }
}
