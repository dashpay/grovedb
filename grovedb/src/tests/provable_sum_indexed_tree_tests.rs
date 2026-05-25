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
        assert_eq!(
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
        assert_eq!(
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
        assert_eq!(
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
        assert_eq!(
            page1,
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
        assert_eq!(
            page2,
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
        assert!(beyond.is_empty());

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
        assert_eq!(plain, pag);
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
        assert_eq!(
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
        assert_eq!(
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
        assert_eq!(exact, vec![(12i64, b"bob".to_vec())]);

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
        assert_eq!(neg, vec![(-7i64, b"carol".to_vec()), (-1, b"eve".to_vec())]);
    }

    #[test]
    fn indexed_sum_range_aggregate_sums_in_range() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_psit_at_test_leaf(&db, b"psit", grove_version);
        psit_populate_known_set(&db, grove_version);

        // Sum in [-1, 12]: -1 + 0 + 5 + 12 = 16.
        let agg = db
            .indexed_sum_range_aggregate([TEST_LEAF, b"psit"].as_ref(), -1, 12, None, grove_version)
            .unwrap()
            .expect("agg");
        assert_eq!(agg, 16);

        // Total sum [i64::MIN, i64::MAX]: -7 + -1 + 0 + 5 + 12 + 100 = 109.
        let total = db
            .indexed_sum_range_aggregate(
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
            .indexed_sum_range_aggregate(
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
            .indexed_sum_range_aggregate(
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
            .indexed_sum_range_aggregate(
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
}
