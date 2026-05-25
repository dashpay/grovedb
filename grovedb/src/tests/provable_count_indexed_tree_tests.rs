//! `ProvableCountIndexedTree` (PCIT) tests.
//!
//! Phase 2/3 coverage for the single-axis (count) indexed tree:
//! - Empty creation + `verify_grovedb` passes.
//! - Single insert / multi-insert / delete + read-back.
//! - Error paths: missing path, wrong tree type, root-path inserts.
//! - Child-type acceptance: Item, SumItem, ItemWithSumItem, Reference,
//!   tree variants (empty), nested PCIT (empty).
//! - Tree-overwrite cleanup: cidx → empty cidx allowed; cidx → non-empty
//!   cidx rejected; cidx → Item allowed with secondary cleanup.
//! - Batch empty creation + cleanup pass.
//! - `verify_grovedb` consistency after every successful mutation.
//! - Secondary-key encoding: count_be ‖ item_key sort order.
//! - Direct-API and batch-API parity for empty creation.
//!
//! These tests intentionally exercise the **public** API surface so
//! changes to the internal split (`_on_transaction` helpers, mirror
//! helpers, post-apply cleanup) are pinned at the integration layer.

#[cfg(test)]
mod tests {
    use grovedb_version::version::GroveVersion;

    use crate::{
        batch::QualifiedGroveDbOp,
        tests::{make_test_grovedb, TEST_LEAF},
        Element, Error,
    };

    // -----------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------

    /// Insert an empty PCIT under `[TEST_LEAF]` at `key` via the
    /// generic `db.insert` API (the direct path, not batch). Asserts
    /// success and that `verify_grovedb` passes after.
    fn insert_empty_pcit(db: &crate::GroveDb, key: &[u8], grove_version: &GroveVersion) {
        db.insert(
            [TEST_LEAF].as_ref(),
            key,
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("PCIT insert should succeed");
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
    // Empty creation / fetch
    // -----------------------------------------------------------------

    #[test]
    fn pcit_empty_creation_via_direct_insert_verify_passes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcit(&db, b"cidx", grove_version);
        let elem = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get should return PCIT");
        assert!(matches!(
            elem,
            Element::ProvableCountIndexedTree(None, None, 0, _)
        ));
    }

    #[test]
    fn pcit_empty_creation_via_batch_verify_passes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"cidx".to_vec(),
                Element::empty_provable_count_indexed_tree(),
            )],
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("batch insert of empty PCIT should succeed");
        let elem = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get returns PCIT");
        assert!(matches!(
            elem,
            Element::ProvableCountIndexedTree(None, None, 0, _)
        ));
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn pcit_empty_creation_with_flags_round_trips() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_provable_count_indexed_tree_with_flags(Some(vec![0xAB])),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("PCIT with flags insert ok");
        let elem = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get");
        match elem {
            Element::ProvableCountIndexedTree(None, None, 0, Some(flags)) => {
                assert_eq!(flags, vec![0xAB]);
            }
            other => panic!("expected PCIT with flags, got {:?}", other),
        }
        assert_verify_passes(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // Direct-API insert: single item, multiple items, delete
    // -----------------------------------------------------------------

    #[test]
    fn pcit_single_insert_then_read_back() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcit(&db, b"cidx", grove_version);
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"row1",
            Element::new_item(b"hello".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert into PCIT");
        let elem = db
            .get([TEST_LEAF, b"cidx"].as_ref(), b"row1", None, grove_version)
            .unwrap()
            .expect("get");
        assert_eq!(elem, Element::new_item(b"hello".to_vec()));
        // Parent count should now be 1.
        let parent = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get PCIT");
        match parent {
            Element::ProvableCountIndexedTree(Some(_), Some(_), 1, _) => {}
            other => panic!(
                "expected count=1 PCIT with both root keys set, got {:?}",
                other
            ),
        }
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn pcit_multiple_inserts_increment_count_and_verify_passes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcit(&db, b"cidx", grove_version);
        for i in 0..10u8 {
            let key = vec![i];
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                &key,
                Element::new_item(vec![i, i]),
                None,
                grove_version,
            )
            .unwrap()
            .expect("PCIT insert");
        }
        let parent = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get PCIT");
        match parent {
            Element::ProvableCountIndexedTree(_, _, c, _) => assert_eq!(c, 10),
            other => panic!("expected PCIT count=10, got {:?}", other),
        }
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn pcit_delete_existing_returns_true_and_decrements_count() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcit(&db, b"cidx", grove_version);
        for i in 0..5u8 {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                &[i],
                Element::new_item(vec![i]),
                None,
                grove_version,
            )
            .unwrap()
            .expect("PCIT insert");
        }
        let removed = db
            .delete_from_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                &[2u8],
                None,
                grove_version,
            )
            .unwrap()
            .expect("PCIT delete");
        assert!(removed, "delete must return true for existing key");
        let parent = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get PCIT");
        match parent {
            Element::ProvableCountIndexedTree(_, _, c, _) => assert_eq!(c, 4),
            other => panic!("expected PCIT count=4, got {:?}", other),
        }
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn pcit_delete_missing_returns_false_and_is_idempotent() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcit(&db, b"cidx", grove_version);
        let removed = db
            .delete_from_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                b"absent",
                None,
                grove_version,
            )
            .unwrap()
            .expect("PCIT delete of missing key");
        assert!(!removed);
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn pcit_overwrite_item_value_keeps_count_constant() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcit(&db, b"cidx", grove_version);
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"row",
            Element::new_item(b"first".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("first insert");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"row",
            Element::new_item(b"second".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("overwrite");
        let elem = db
            .get([TEST_LEAF, b"cidx"].as_ref(), b"row", None, grove_version)
            .unwrap()
            .expect("get");
        assert_eq!(elem, Element::new_item(b"second".to_vec()));
        let parent = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get PCIT");
        match parent {
            Element::ProvableCountIndexedTree(_, _, c, _) => assert_eq!(c, 1),
            other => panic!("expected count=1 PCIT, got {:?}", other),
        }
        assert_verify_passes(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // Error paths: wrong target, root-path inserts, invalid items
    // -----------------------------------------------------------------

    #[test]
    fn pcit_insert_rejects_non_pcit_target() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let result = db
            .insert_into_count_indexed_tree(
                [TEST_LEAF].as_ref(),
                b"row",
                Element::new_item(b"x".to_vec()),
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(Error::InvalidPath(msg)) => assert!(
                msg.contains("CountIndexedTree") || msg.contains("cidx"),
                "expected InvalidPath with cidx context, got: {msg}"
            ),
            other => panic!("expected InvalidPath, got {:?}", other),
        }
    }

    #[test]
    fn pcit_delete_rejects_non_pcit_target() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let result = db
            .delete_from_count_indexed_tree([TEST_LEAF].as_ref(), b"row", None, grove_version)
            .unwrap();
        assert!(result.is_err(), "delete on non-PCIT must fail");
    }

    #[test]
    fn pcit_insert_at_root_path_is_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        // Root path has no parent => derive_parent returns None.
        let empty_path: [&[u8]; 0] = [];
        let result = db
            .insert_into_count_indexed_tree(
                empty_path.as_ref(),
                b"row",
                Element::new_item(b"x".to_vec()),
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(Error::InvalidPath(msg)) => assert!(
                msg.contains("root path"),
                "expected root-path InvalidPath, got: {msg}"
            ),
            other => panic!("expected InvalidPath (root), got {:?}", other),
        }
    }

    #[test]
    fn pcit_delete_at_root_path_is_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let empty_path: [&[u8]; 0] = [];
        let result = db
            .delete_from_count_indexed_tree(empty_path.as_ref(), b"row", None, grove_version)
            .unwrap();
        match result {
            Err(Error::InvalidPath(msg)) => assert!(
                msg.contains("root path"),
                "expected root-path InvalidPath, got: {msg}"
            ),
            other => panic!("expected InvalidPath (root), got {:?}", other),
        }
    }

    #[test]
    fn pcit_insert_rejects_non_empty_pcit_child() {
        // Direct-API insert of a PCIT child must reject non-empty
        // claims (the API short-circuits child roots to NULL_HASH).
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcit(&db, b"cidx", grove_version);
        let bogus = Element::new_provable_count_indexed_tree_with_root_keys_and_count_value(
            Some(b"bogus".to_vec()),
            None,
            0,
            None,
        );
        let result = db
            .insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                b"nested",
                bogus,
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(Error::NotSupported(msg)) => assert!(
                msg.contains("EMPTY cidx"),
                "expected EMPTY cidx requirement, got: {msg}"
            ),
            other => panic!("expected NotSupported, got {:?}", other),
        }
    }

    #[test]
    fn pcit_insert_rejects_non_empty_tree_child() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcit(&db, b"cidx", grove_version);
        let bogus = Element::new_tree(Some(b"bogus".to_vec()));
        let result = db
            .insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                b"nested",
                bogus,
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(Error::NotSupported(msg)) => assert!(
                msg.contains("EMPTY tree"),
                "expected EMPTY tree requirement, got: {msg}"
            ),
            other => panic!("expected NotSupported, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------
    // Tree-overwrite cleanup
    // -----------------------------------------------------------------

    #[test]
    fn pcit_batch_overwrite_existing_pcit_with_empty_pcit_clears_secondary() {
        // Per `inspect_cidx_overwrite`: cidx → empty cidx is the safe
        // subset and must succeed via batch (post-apply cleanup
        // clears the old secondary namespace at
        // Blake3(primary_prefix ‖ 0x01)).
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcit(&db, b"cidx", grove_version);
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"row",
            Element::new_item(b"x".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");
        // Overwrite the cidx in-place with an empty cidx via batch.
        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"cidx".to_vec(),
                Element::empty_provable_count_indexed_tree(),
            )],
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("batch overwrite cidx with empty cidx ok");
        let parent = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get");
        assert!(matches!(
            parent,
            Element::ProvableCountIndexedTree(None, None, 0, _)
        ));
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn pcit_batch_overwrite_existing_pcit_with_non_empty_pcit_is_rejected() {
        // cidx → non-empty cidx must be rejected by
        // `inspect_cidx_overwrite` (storage-pointer ambiguity: the new
        // root keys would refer to on-disk data that post-apply
        // cleanup of the OLD cidx also clears).
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcit(&db, b"cidx", grove_version);
        let non_empty = Element::new_provable_count_indexed_tree_with_root_keys_and_count_value(
            Some(b"bogus".to_vec()),
            None,
            7,
            None,
        );
        let result = db
            .apply_batch(
                vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"cidx".to_vec(),
                    non_empty,
                )],
                None,
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(Error::NotSupported(msg)) => assert!(
                msg.contains("NON-EMPTY cidx") || msg.contains("cidx"),
                "expected NotSupported, got: {msg}"
            ),
            other => panic!("expected NotSupported, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------
    // Nested PCIT
    // -----------------------------------------------------------------

    #[test]
    fn pcit_nested_pcit_under_pcit_via_direct_api() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcit(&db, b"outer", grove_version);
        // Insert an empty PCIT as a child of the outer PCIT via the
        // dedicated direct API (the nested cidx Op::PutLayered path).
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer"].as_ref(),
            b"inner",
            Element::empty_provable_count_indexed_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("nested PCIT insert");
        let inner = db
            .get(
                [TEST_LEAF, b"outer"].as_ref(),
                b"inner",
                None,
                grove_version,
            )
            .unwrap()
            .expect("get inner PCIT");
        assert!(matches!(
            inner,
            Element::ProvableCountIndexedTree(None, None, 0, _)
        ));
        assert_verify_passes(&db, grove_version);

        // Insert one item into the inner PCIT.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer", b"inner"].as_ref(),
            b"row",
            Element::new_item(b"y".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("inner insert");
        let inner = db
            .get(
                [TEST_LEAF, b"outer"].as_ref(),
                b"inner",
                None,
                grove_version,
            )
            .unwrap()
            .expect("get inner");
        match inner {
            Element::ProvableCountIndexedTree(_, _, c, _) => assert_eq!(c, 1),
            other => panic!("expected inner count=1, got {:?}", other),
        }
        // Outer count counts entries in its primary — the inner cidx
        // contributes 1 (it's a single entry from the outer's view).
        let outer = db
            .get([TEST_LEAF].as_ref(), b"outer", None, grove_version)
            .unwrap()
            .expect("get outer");
        match outer {
            Element::ProvableCountIndexedTree(_, _, c, _) => assert_eq!(c, 1),
            other => panic!("expected outer count=1, got {:?}", other),
        }
        assert_verify_passes(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // Item-type acceptance
    // -----------------------------------------------------------------

    #[test]
    fn pcit_rejects_sum_item_child() {
        // PCIT's primary mirrors `ProvableCountTree`: it does NOT
        // allow sum items as children. The merk-layer guard
        // (`allows_sum_item`) returns false for the PCIT primary, so
        // the insert fails with a sum-item rejection.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcit(&db, b"cidx", grove_version);
        let result = db
            .insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                b"row",
                Element::new_sum_item(42),
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            result.is_err(),
            "PCIT must reject a SumItem child, got: {:?}",
            result
        );
    }

    #[test]
    fn pcit_rejects_item_with_sum_item_child() {
        // Same reasoning as `pcit_rejects_sum_item_child`: PCIT
        // primary is not sum-bearing.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcit(&db, b"cidx", grove_version);
        let v = Element::new_item_with_sum_item(b"v".to_vec(), 7);
        let result = db
            .insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                b"row",
                v,
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            result.is_err(),
            "PCIT must reject an ItemWithSumItem child, got: {:?}",
            result
        );
    }

    #[test]
    fn pcit_accepts_empty_tree_child() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcit(&db, b"cidx", grove_version);
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"row",
            Element::empty_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("empty tree insert");
        let elem = db
            .get([TEST_LEAF, b"cidx"].as_ref(), b"row", None, grove_version)
            .unwrap()
            .expect("get");
        assert!(matches!(elem, Element::Tree(None, _)));
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn pcit_accepts_empty_sum_tree_child() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcit(&db, b"cidx", grove_version);
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"row",
            Element::empty_sum_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("empty sum tree insert");
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn pcit_accepts_empty_count_tree_child() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcit(&db, b"cidx", grove_version);
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"row",
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("empty count tree insert");
        assert_verify_passes(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // Secondary index ordering / sort key
    // -----------------------------------------------------------------

    #[test]
    fn pcit_inserts_in_arbitrary_order_have_count_be_secondary_order() {
        // The secondary is keyed by `count_be_at_insert ‖ key`. The
        // count at the time each row is inserted is i, so the secondary
        // ordering matches insertion order. This pins the encoder
        // contract.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcit(&db, b"cidx", grove_version);
        // Insert keys in non-ascending order — secondary index will
        // still order them by insertion count, not primary key.
        let keys: &[&[u8]] = &[b"zeta", b"alpha", b"mu", b"beta"];
        for k in keys {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::new_item(k.to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        let parent = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get PCIT");
        match parent {
            Element::ProvableCountIndexedTree(_, _, c, _) => assert_eq!(c, 4),
            other => panic!("expected count=4 PCIT, got {:?}", other),
        }
        assert_verify_passes(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // Verify after delete sequences
    // -----------------------------------------------------------------

    #[test]
    fn pcit_delete_all_then_reinsert_round_trips() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcit(&db, b"cidx", grove_version);
        for i in 0..4u8 {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                &[i],
                Element::new_item(vec![i]),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        // Delete all.
        for i in 0..4u8 {
            let removed = db
                .delete_from_count_indexed_tree(
                    [TEST_LEAF, b"cidx"].as_ref(),
                    &[i],
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("delete");
            assert!(removed);
        }
        let parent = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get PCIT");
        match parent {
            Element::ProvableCountIndexedTree(_, _, c, _) => assert_eq!(c, 0),
            other => panic!("expected count=0 PCIT, got {:?}", other),
        }
        assert_verify_passes(&db, grove_version);

        // Reinsert one entry — should work.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"again",
            Element::new_item(b"x".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("reinsert");
        let parent = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get PCIT");
        match parent {
            Element::ProvableCountIndexedTree(_, _, c, _) => assert_eq!(c, 1),
            other => panic!("expected count=1 PCIT, got {:?}", other),
        }
        assert_verify_passes(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // Batch: fresh-cidx-and-children rejection
    // -----------------------------------------------------------------

    #[test]
    fn pcit_batch_rejects_creating_pcit_with_children_in_same_batch() {
        // Phase 1 invariant: a batch that BOTH creates a PCIT and
        // contains writes under its path is rejected with
        // NotSupported. Workaround is splitting into two batches.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let result = db
            .apply_batch(
                vec![
                    QualifiedGroveDbOp::insert_or_replace_op(
                        vec![TEST_LEAF.to_vec()],
                        b"cidx".to_vec(),
                        Element::empty_provable_count_indexed_tree(),
                    ),
                    QualifiedGroveDbOp::insert_or_replace_op(
                        vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                        b"row".to_vec(),
                        Element::new_item(b"x".to_vec()),
                    ),
                ],
                None,
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(Error::NotSupported(msg)) => assert!(
                msg.contains("freshly-inserted") || msg.contains("CountIndexedTree"),
                "expected fresh-cidx rejection, got: {msg}"
            ),
            other => panic!("expected NotSupported, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------
    // verify_grovedb is non-empty grovedb structure round-trip
    // -----------------------------------------------------------------

    #[test]
    fn pcit_verify_after_mixed_op_sequence() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcit(&db, b"a", grove_version);
        insert_empty_pcit(&db, b"b", grove_version);
        // Populate both.
        for cidx_key in [b"a".as_ref(), b"b".as_ref()] {
            for i in 0..3u8 {
                db.insert_into_count_indexed_tree(
                    [TEST_LEAF, cidx_key].as_ref(),
                    &[i],
                    Element::new_item(vec![i, 0]),
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert");
            }
        }
        // Delete one item from each.
        db.delete_from_count_indexed_tree([TEST_LEAF, b"a"].as_ref(), &[1u8], None, grove_version)
            .unwrap()
            .expect("delete");
        db.delete_from_count_indexed_tree([TEST_LEAF, b"b"].as_ref(), &[0u8], None, grove_version)
            .unwrap()
            .expect("delete");
        assert_verify_passes(&db, grove_version);

        // Final counts: a=2, b=2.
        for cidx_key in [b"a".as_ref(), b"b".as_ref()] {
            let parent = db
                .get([TEST_LEAF].as_ref(), cidx_key, None, grove_version)
                .unwrap()
                .expect("get");
            match parent {
                Element::ProvableCountIndexedTree(_, _, c, _) => assert_eq!(c, 2),
                other => panic!("expected count=2, got {:?}", other),
            }
        }
    }

    // -----------------------------------------------------------------
    // Item-key length bound
    // -----------------------------------------------------------------

    #[test]
    fn pcit_insert_rejects_oversized_item_key() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcit(&db, b"cidx", grove_version);
        // The secondary key is `count_be (8 bytes) ‖ item_key`; the
        // ceiling is 247 bytes for the item key to keep the merk
        // 255-byte limit.
        let too_long = vec![0u8; 248];
        let result = db
            .insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                &too_long,
                Element::new_item(b"x".to_vec()),
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            result.is_err(),
            "PCIT insert must reject keys exceeding the secondary-key ceiling"
        );
    }

    // -----------------------------------------------------------------
    // Phase 3: indexed_count_* direct query APIs over PCIT
    // -----------------------------------------------------------------

    /// Insert a CountTree child with explicit count_value under
    /// `[TEST_LEAF, "cidx"]`. The PCIT's secondary key is
    /// `count_be ‖ item_key` so different count_values produce
    /// distinct, ordered secondary entries.
    fn pcit_insert_count_child(
        db: &crate::GroveDb,
        item_key: &[u8],
        count_value: u64,
        grove_version: &GroveVersion,
    ) {
        let count_tree =
            Element::new_count_tree_with_flags_and_count_value(None, count_value, None);
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            item_key,
            count_tree,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert count-tree child");
    }

    /// Populate the PCIT under `[TEST_LEAF, "cidx"]` with a known set
    /// of `(key, count)` pairs:
    ///   alice=5, bob=12, carol=1, dave=7, eve=20.
    fn pcit_populate_known_set(db: &crate::GroveDb, grove_version: &GroveVersion) {
        for (k, c) in [
            (b"alice".as_ref(), 5u64),
            (b"bob", 12),
            (b"carol", 1),
            (b"dave", 7),
            (b"eve", 20),
        ] {
            pcit_insert_count_child(db, k, c, grove_version);
        }
    }

    #[test]
    fn indexed_count_top_k_on_pcit_descending_returns_highest_first() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcit(&db, b"cidx", grove_version);
        pcit_populate_known_set(&db, grove_version);

        // Top 3 descending: eve(20), bob(12), dave(7).
        let top3 = db
            .indexed_count_top_k([TEST_LEAF, b"cidx"].as_ref(), 3, true, None, grove_version)
            .unwrap()
            .expect("top-k descending");
        assert_eq!(
            top3,
            vec![
                (20u64, b"eve".to_vec()),
                (12u64, b"bob".to_vec()),
                (7u64, b"dave".to_vec()),
            ]
        );

        // Top 2 ascending: carol(1), alice(5).
        let bottom2 = db
            .indexed_count_top_k([TEST_LEAF, b"cidx"].as_ref(), 2, false, None, grove_version)
            .unwrap()
            .expect("top-k ascending");
        assert_eq!(
            bottom2,
            vec![(1u64, b"carol".to_vec()), (5u64, b"alice".to_vec())]
        );
    }

    #[test]
    fn indexed_count_top_k_on_empty_pcit_returns_empty() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcit(&db, b"cidx", grove_version);
        let top = db
            .indexed_count_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("top-k empty");
        assert!(top.is_empty());
    }

    #[test]
    fn indexed_count_top_k_k_zero_returns_empty() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcit(&db, b"cidx", grove_version);
        pcit_populate_known_set(&db, grove_version);
        let top = db
            .indexed_count_top_k([TEST_LEAF, b"cidx"].as_ref(), 0, true, None, grove_version)
            .unwrap()
            .expect("k=0");
        assert!(top.is_empty());
    }

    #[test]
    fn indexed_count_top_k_paginated_pages_through_dataset() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcit(&db, b"cidx", grove_version);
        pcit_populate_known_set(&db, grove_version);

        // Descending full scan: eve(20), bob(12), dave(7), alice(5), carol(1).
        // Page 1 (offset=0, k=2): eve, bob.
        let page1 = db
            .indexed_count_top_k_paginated(
                [TEST_LEAF, b"cidx"].as_ref(),
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
            vec![(20u64, b"eve".to_vec()), (12u64, b"bob".to_vec())]
        );

        // Page 2 (offset=2, k=2): dave, alice.
        let page2 = db
            .indexed_count_top_k_paginated(
                [TEST_LEAF, b"cidx"].as_ref(),
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
            vec![(7u64, b"dave".to_vec()), (5u64, b"alice".to_vec())]
        );

        // Page 3 (offset=4, k=2): just carol; second slot unfilled.
        let page3 = db
            .indexed_count_top_k_paginated(
                [TEST_LEAF, b"cidx"].as_ref(),
                2,
                4,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("page 3");
        assert_eq!(page3, vec![(1u64, b"carol".to_vec())]);

        // Offset beyond total → empty.
        let beyond = db
            .indexed_count_top_k_paginated(
                [TEST_LEAF, b"cidx"].as_ref(),
                5,
                10,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("offset beyond");
        assert!(beyond.is_empty());

        // offset=0 ≡ plain top_k.
        let plain = db
            .indexed_count_top_k([TEST_LEAF, b"cidx"].as_ref(), 3, true, None, grove_version)
            .unwrap()
            .expect("plain");
        let paginated = db
            .indexed_count_top_k_paginated(
                [TEST_LEAF, b"cidx"].as_ref(),
                3,
                0,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("paginated offset 0");
        assert_eq!(plain, paginated);
    }

    #[test]
    fn indexed_count_range_inclusive_bounds_match_plain_filter() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcit(&db, b"cidx", grove_version);
        pcit_populate_known_set(&db, grove_version);

        // [5, 12] ascending should include alice(5), dave(7), bob(12).
        let in_range = db
            .indexed_count_range(
                [TEST_LEAF, b"cidx"].as_ref(),
                5,
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
                (5u64, b"alice".to_vec()),
                (7u64, b"dave".to_vec()),
                (12u64, b"bob".to_vec()),
            ]
        );

        // Same range descending: reversed.
        let in_range_desc = db
            .indexed_count_range(
                [TEST_LEAF, b"cidx"].as_ref(),
                5,
                12,
                true,
                100,
                None,
                grove_version,
            )
            .unwrap()
            .expect("range desc");
        assert_eq!(
            in_range_desc,
            vec![
                (12u64, b"bob".to_vec()),
                (7u64, b"dave".to_vec()),
                (5u64, b"alice".to_vec()),
            ]
        );

        // Exact match (lo == hi): only bob(12).
        let exact = db
            .indexed_count_range(
                [TEST_LEAF, b"cidx"].as_ref(),
                12,
                12,
                false,
                100,
                None,
                grove_version,
            )
            .unwrap()
            .expect("exact");
        assert_eq!(exact, vec![(12u64, b"bob".to_vec())]);

        // lo > hi: empty.
        let empty = db
            .indexed_count_range(
                [TEST_LEAF, b"cidx"].as_ref(),
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

        // Full scan [0, u64::MAX].
        let full = db
            .indexed_count_range(
                [TEST_LEAF, b"cidx"].as_ref(),
                0,
                u64::MAX,
                false,
                100,
                None,
                grove_version,
            )
            .unwrap()
            .expect("full");
        assert_eq!(full.len(), 5);

        // limit=0 returns empty.
        let zero_limit = db
            .indexed_count_range(
                [TEST_LEAF, b"cidx"].as_ref(),
                0,
                u64::MAX,
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
    fn indexed_count_range_aggregate_counts_in_range() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcit(&db, b"cidx", grove_version);
        pcit_populate_known_set(&db, grove_version);

        // [5, 12]: alice + dave + bob = 3 entries.
        let count = db
            .indexed_count_range_aggregate(
                [TEST_LEAF, b"cidx"].as_ref(),
                5,
                12,
                None,
                grove_version,
            )
            .unwrap()
            .expect("agg");
        assert_eq!(count, 3);

        // [0, u64::MAX]: total count of entries (5).
        let total = db
            .indexed_count_range_aggregate(
                [TEST_LEAF, b"cidx"].as_ref(),
                0,
                u64::MAX,
                None,
                grove_version,
            )
            .unwrap()
            .expect("total");
        assert_eq!(total, 5);

        // [100, 200]: nothing in this range → 0.
        let none = db
            .indexed_count_range_aggregate(
                [TEST_LEAF, b"cidx"].as_ref(),
                100,
                200,
                None,
                grove_version,
            )
            .unwrap()
            .expect("none");
        assert_eq!(none, 0);

        // lo > hi: 0 (degenerate).
        let degenerate = db
            .indexed_count_range_aggregate(
                [TEST_LEAF, b"cidx"].as_ref(),
                100,
                10,
                None,
                grove_version,
            )
            .unwrap()
            .expect("degenerate");
        assert_eq!(degenerate, 0);
    }

    // ---- axis-compatibility rejection on PCIT ----

    #[test]
    fn indexed_sum_top_k_on_pcit_returns_invalid_path() {
        // The sum axis is NOT indexed on a PCIT; the API must reject
        // with InvalidPath rather than producing nonsense results from
        // the count secondary.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcit(&db, b"cidx", grove_version);
        let result = db
            .indexed_sum_top_k([TEST_LEAF, b"cidx"].as_ref(), 5, true, None, grove_version)
            .unwrap();
        match result {
            Err(Error::InvalidPath(msg)) => {
                assert!(
                    msg.contains("Sum") && msg.contains("not indexed"),
                    "expected axis-compat rejection, got: {msg}"
                );
            }
            other => panic!("expected InvalidPath, got {:?}", other),
        }
    }

    #[test]
    fn indexed_avg_top_k_on_pcit_returns_invalid_path() {
        // The avg axis is only available on PCPSIT; PCIT rejects.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcit(&db, b"cidx", grove_version);
        let result = db
            .indexed_avg_top_k([TEST_LEAF, b"cidx"].as_ref(), 5, true, None, grove_version)
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

    // ---- legacy alias: count_indexed_top_k delegates to indexed_count_top_k ----

    #[test]
    #[allow(deprecated)]
    fn legacy_count_indexed_top_k_alias_matches_new_indexed_count_top_k() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcit(&db, b"cidx", grove_version);
        pcit_populate_known_set(&db, grove_version);

        let new_api = db
            .indexed_count_top_k([TEST_LEAF, b"cidx"].as_ref(), 3, true, None, grove_version)
            .unwrap()
            .expect("new");
        let legacy = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 3, true, None, grove_version)
            .unwrap()
            .expect("legacy");
        assert_eq!(new_api, legacy);
    }

    #[test]
    #[allow(deprecated)]
    fn legacy_count_indexed_range_aggregate_alias_matches_new() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcit(&db, b"cidx", grove_version);
        pcit_populate_known_set(&db, grove_version);

        let new_api = db
            .indexed_count_range_aggregate(
                [TEST_LEAF, b"cidx"].as_ref(),
                5,
                12,
                None,
                grove_version,
            )
            .unwrap()
            .expect("new");
        let legacy = db
            .count_indexed_count_range_aggregate(
                [TEST_LEAF, b"cidx"].as_ref(),
                5,
                12,
                None,
                grove_version,
            )
            .unwrap()
            .expect("legacy");
        assert_eq!(new_api, legacy);
    }

    // -----------------------------------------------------------------
    // Depth > 1 propagation paths
    // -----------------------------------------------------------------

    #[test]
    fn pcit_depth_2_under_tree_propagates_count_and_verifies() {
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
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cidx");
        for k in [b"a".as_slice(), b"b", b"c", b"d"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"parent", b"cidx"].as_ref(),
                k,
                Element::new_item(b"v".to_vec()),
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
                b"cidx",
                None,
                grove_version,
            )
            .unwrap()
            .expect("get");
        match elem.underlying() {
            Element::ProvableCountIndexedTree(_, _, c, _) => assert_eq!(*c, 4),
            other => panic!("expected PCIT, got {:?}", other),
        }
    }

    #[test]
    fn pcit_depth_3_propagates_count() {
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
        db.insert(
            [TEST_LEAF, b"l1", b"l2"].as_ref(),
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cidx");
        for k in [b"x".as_slice(), b"y", b"z"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"l1", b"l2", b"cidx"].as_ref(),
                k,
                Element::new_item(b"v".to_vec()),
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
                b"cidx",
                None,
                grove_version,
            )
            .unwrap()
            .expect("get");
        match elem.underlying() {
            Element::ProvableCountIndexedTree(_, _, c, _) => assert_eq!(*c, 3),
            other => panic!("expected PCIT, got {:?}", other),
        }
    }

    #[test]
    fn pcit_delete_then_reinsert_at_depth_2_consistent() {
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
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cidx");
        for k in [b"a".as_slice(), b"b"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"parent", b"cidx"].as_ref(),
                k,
                Element::new_item(b"v".to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        db.delete_from_count_indexed_tree(
            [TEST_LEAF, b"parent", b"cidx"].as_ref(),
            b"a",
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"parent", b"cidx"].as_ref(),
            b"c",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("c");
        assert_verify_passes(&db, grove_version);
        let elem = db
            .get(
                [TEST_LEAF, b"parent"].as_ref(),
                b"cidx",
                None,
                grove_version,
            )
            .unwrap()
            .expect("get");
        match elem.underlying() {
            Element::ProvableCountIndexedTree(_, _, c, _) => assert_eq!(*c, 2),
            other => panic!("expected PCIT, got {:?}", other),
        }
    }

    /// Direct `db.insert` of a NON-EMPTY PCIT validates the claimed
    /// primary + secondary root keys against on-disk state and succeeds
    /// when they match. Exercises the non-empty success path in
    /// `operations/insert/mod.rs`.
    #[test]
    fn pcit_direct_insert_non_empty_with_matching_roots_succeeds() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcit",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("create PCIT");
        for k in [b"a".as_ref(), b"b"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"pcit"].as_ref(),
                k,
                Element::new_item(b"v".to_vec()),
                None,
                v,
            )
            .unwrap()
            .expect("populate");
        }
        let populated = db
            .get([TEST_LEAF].as_ref(), b"pcit", None, v)
            .unwrap()
            .expect("get populated PCIT");
        assert!(matches!(
            populated,
            Element::ProvableCountIndexedTree(Some(_), Some(_), 2, _)
        ));
        let opts = crate::operations::insert::InsertOptions {
            validate_insertion_does_not_override: false,
            validate_insertion_does_not_override_tree: false,
            base_root_storage_is_free: true,
        };
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcit",
            populated,
            Some(opts),
            None,
            v,
        )
        .unwrap()
        .expect("re-insert non-empty PCIT with matching roots");
        assert!(db
            .verify_grovedb(None, true, true, v)
            .expect("verify_grovedb")
            .is_empty());
    }

    /// Direct `db.insert` of a non-empty PCIT with a mismatched secondary
    /// root key must be rejected.
    #[test]
    fn pcit_direct_insert_non_empty_with_mismatched_secondary_root_rejected() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcit",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("create PCIT");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"pcit"].as_ref(),
            b"a",
            Element::new_item(b"v".to_vec()),
            None,
            v,
        )
        .unwrap()
        .expect("populate");
        let populated = db
            .get([TEST_LEAF].as_ref(), b"pcit", None, v)
            .unwrap()
            .expect("get");
        let Element::ProvableCountIndexedTree(primary, _sec, count, flags) = populated else {
            panic!("expected PCIT");
        };
        // Wrong secondary root key.
        let tampered =
            Element::ProvableCountIndexedTree(primary, Some(vec![0xCD; 32]), count, flags);
        let opts = crate::operations::insert::InsertOptions {
            validate_insertion_does_not_override: false,
            validate_insertion_does_not_override_tree: false,
            base_root_storage_is_free: true,
        };
        let res = db
            .insert([TEST_LEAF].as_ref(), b"pcit", tampered, Some(opts), None, v)
            .unwrap();
        assert!(
            matches!(res, Err(Error::InvalidInput(_))),
            "mismatched secondary root key must be rejected; got {res:?}"
        );
    }
}
