//! What happens when a PCIT's count-ordered secondary index stops agreeing
//! with its primary — how the drift is repaired, and how the read APIs behave
//! against a malformed row.
//!
//! The reconcile API is the repair door for a PCIT whose count-ordered
//! secondary has drifted from its primary: it walks the primary, derives the
//! secondary keyset that *should* exist, deletes the rows that should not,
//! inserts the rows that are missing, rebinds the parent element via the H1-A
//! `combine_hash_three` composition and propagates the change to the root.
//!
//! Nothing else in the crate calls it, so every one of its branches was
//! unexercised. These tests drive all four outcomes — no-op, orphan removal,
//! missing-row repair, and the three refusal paths — and pin them on the
//! discriminating signals: `verify_grovedb` issue keys, the exact
//! `indexed_count_top_k` listing, and root-hash equality/inequality.

#[cfg(test)]
mod tests {
    use grovedb_element::indexed::IndexAxis;
    use grovedb_merk::{
        element::{
            delete::ElementDeleteFromStorageExtensions, get::ElementFetchFromStorageExtensions,
            insert::ElementInsertToStorageExtensions,
        },
        tree_type::TreeType,
    };
    use grovedb_path::SubtreePath;
    use grovedb_storage::{Storage, StorageBatch};
    use grovedb_version::version::GroveVersion;

    use crate::IndexedAxisEntrySliceExt;

    use crate::{
        tests::{common::EMPTY_PATH, make_test_grovedb, SuiteBatch, TempGroveDb, TEST_LEAF},
        Element, Engine, Error,
    };

    // -----------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------

    #[cfg(grovedb_test_engine = "rocksdb")]
    fn secondary_key(count: u64, item_key: &[u8]) -> Vec<u8> {
        let mut k = Vec::with_capacity(8 + item_key.len());
        k.extend_from_slice(&count.to_be_bytes());
        k.extend_from_slice(item_key);
        k
    }

    fn make_pcit_with(db: &TempGroveDb, key: &[u8], items: &[&[u8]], gv: &GroveVersion) {
        db.insert(
            [TEST_LEAF].as_ref(),
            key,
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("create PCIT");
        for item in items {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, key].as_ref(),
                item,
                Element::new_item(item.to_vec()),
                None,
                gv,
            )
            .unwrap()
            .expect("insert into PCIT");
        }
    }

    fn issue_keys(db: &TempGroveDb, gv: &GroveVersion) -> Vec<Vec<Vec<u8>>> {
        db.verify_grovedb(None, true, true, gv)
            .expect("verify_grovedb must not hard-error")
            .into_keys()
            .collect()
    }

    fn assert_clean(db: &TempGroveDb, gv: &GroveVersion) {
        let issues = issue_keys(db, gv);
        assert!(issues.is_empty(), "verify_grovedb reported {issues:?}");
    }

    /// Read the PCIT element's current `secondary_root_key` from its parent.
    fn read_secondary_root_key(
        db: &TempGroveDb,
        pcit_path: &[&[u8]],
        gv: &GroveVersion,
    ) -> Option<Vec<u8>> {
        let tx = db.start_transaction();
        let batch = SuiteBatch::default();
        let owned: Vec<&[u8]> = pcit_path.to_vec();
        let path: SubtreePath<&[u8]> = owned.as_slice().into();
        let (parent_path, pcit_key) = path.derive_parent().expect("non-root PCIT");
        let parent_merk = db
            .open_transactional_merk_at_path(parent_path, &tx, Some(&batch), gv)
            .unwrap()
            .expect("open parent merk");
        let element = Element::get(&parent_merk, pcit_key, true, gv)
            .unwrap()
            .expect("PCIT element");
        match element.underlying() {
            Element::ProvableCountIndexedTree(_, s, ..) => s.clone(),
            other => panic!("not a PCIT element: {other:?}"),
        }
    }

    /// Drift the count secondary of a PCIT away from its primary while keeping
    /// the tree *structurally* sound: the rows are added/removed directly, then
    /// the parent element is rebound to the secondary's new `(root_hash,
    /// root_key)` and the change is propagated to the GroveDB root.
    ///
    /// Rebinding matters. Mutating the secondary without it leaves the parent
    /// pointing at a root key that no longer describes the tree, which is a torn
    /// state no repair can read — reconcile's `Element::delete` would traverse
    /// from a stale root, find nothing, and leave the row behind. What reconcile
    /// is specified to fix is a *coherent* secondary carrying the wrong content,
    /// which is what this produces.
    #[cfg(grovedb_test_engine = "rocksdb")]
    fn drift_secondary(
        db: &TempGroveDb,
        pcit_path: &[&[u8]],
        insert: &[Vec<u8>],
        delete: &[Vec<u8>],
        gv: &GroveVersion,
    ) {
        use grovedb_merk::element::reconstruct::ElementReconstructExtensions;

        let tx = db.start_transaction();
        let batch = StorageBatch::new();
        let owned: Vec<&[u8]> = pcit_path.to_vec();
        let path: SubtreePath<&[u8]> = owned.as_slice().into();
        let (parent_path, pcit_key) = path.derive_parent().expect("non-root PCIT");

        let (primary_root_hash, primary_root_key, primary_aggregate) = {
            let primary = db
                .open_transactional_merk_at_path(path.clone(), &tx, Some(&batch), gv)
                .unwrap()
                .expect("open PCIT primary");
            primary
                .root_hash_key_and_aggregate_data()
                .unwrap()
                .expect("primary root state")
        };

        let mut parent_merk = db
            .open_transactional_merk_at_path(parent_path.clone(), &tx, Some(&batch), gv)
            .unwrap()
            .expect("open parent merk");
        let pcit_element = Element::get(&parent_merk, pcit_key, true, gv)
            .unwrap()
            .expect("PCIT element");
        let secondary_root_key = match pcit_element.underlying() {
            Element::ProvableCountIndexedTree(_, s, ..) => s.clone(),
            other => panic!("not a PCIT element: {other:?}"),
        };

        let (secondary_root_hash, secondary_root_key_after) = {
            let mut secondary = db
                .open_indexed_secondary_at_path(
                    path.clone(),
                    IndexAxis::Count,
                    secondary_root_key,
                    &tx,
                    Some(&batch),
                    gv,
                )
                .unwrap()
                .expect("open count secondary");
            for key in delete {
                Element::delete(
                    &mut secondary,
                    key.as_slice(),
                    None,
                    false,
                    TreeType::ProvableCountProvableSumTree,
                    gv,
                )
                .unwrap()
                .expect("delete secondary row");
            }
            for key in insert {
                Element::new_item(Vec::new())
                    .insert(&mut secondary, key.as_slice(), None, gv)
                    .unwrap()
                    .expect("insert secondary row");
            }
            let (hash, root_key, _) = secondary
                .root_hash_key_and_aggregate_data()
                .unwrap()
                .expect("secondary root state");
            (hash, root_key)
        };

        let rebound = pcit_element
            .reconstruct_with_two_root_keys(
                primary_root_key,
                secondary_root_key_after,
                primary_aggregate,
            )
            .expect("reconstruct PCIT element");
        rebound
            .insert_count_indexed_subtree(
                &mut parent_merk,
                pcit_key,
                primary_root_hash,
                secondary_root_hash,
                None,
                gv,
            )
            .unwrap()
            .expect("rebind PCIT element");

        let mut merk_cache = std::collections::HashMap::new();
        merk_cache.insert(parent_path.clone(), parent_merk);
        db.propagate_changes_with_transaction(merk_cache, parent_path, &tx, &batch, gv)
            .unwrap()
            .expect("propagate rebind");

        db.db
            .storage()
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .expect("commit secondary drift");
        tx.commit().expect("tx commit");
    }

    // -----------------------------------------------------------------
    // No-op reconcile
    // -----------------------------------------------------------------

    /// Reconciling a secondary that is already correct must be byte-identical:
    /// no rows deleted, none inserted, and the rebound parent element must hash
    /// to exactly the same root. This is the documented "producing identical
    /// bytes" contract, and it is what makes the repair safe to run blind.
    #[test]
    fn reconcile_of_a_healthy_secondary_leaves_the_root_hash_untouched() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        make_pcit_with(&db, b"pcit", &[b"a", b"b", b"c"], gv);
        assert_clean(&db, gv);

        let before = db.root_hash(None, gv).unwrap().expect("root hash");
        let secondary_root_before = read_secondary_root_key(&db, &[TEST_LEAF, b"pcit"], gv);

        db.reconcile_indexed_tree_secondaries([TEST_LEAF, b"pcit"].as_ref(), None, gv)
            .unwrap()
            .expect("reconcile a healthy secondary");

        let after = db.root_hash(None, gv).unwrap().expect("root hash");
        assert_eq!(
            before, after,
            "reconciling an already-correct secondary must not move the root hash"
        );
        assert_eq!(
            secondary_root_before,
            read_secondary_root_key(&db, &[TEST_LEAF, b"pcit"], gv),
            "the secondary root key must be unchanged too"
        );
        assert_clean(&db, gv);
        // Running it twice must also be stable.
        db.reconcile_indexed_tree_secondaries([TEST_LEAF, b"pcit"].as_ref(), None, gv)
            .unwrap()
            .expect("second reconcile");
        assert_eq!(
            after,
            db.root_hash(None, gv).unwrap().expect("root hash"),
            "reconcile must be idempotent"
        );
    }

    // -----------------------------------------------------------------
    // Repair: delete rows the primary does not justify
    // -----------------------------------------------------------------

    /// A secondary row with no matching primary entry (`__cidx_secondary_orphan__`)
    /// must be deleted by reconcile.
    ///
    /// Note the repaired root hash is asserted against the *drifted* one, not the
    /// pre-drift one: reconcile rebuilds the secondary by replaying the desired
    /// keyset, so the resulting AVL shape is the canonical one for that keyset
    /// rather than whatever shape the incremental inserts happened to leave
    /// behind. `reconcile_is_shape_deterministic_regardless_of_how_the_index_drifted`
    /// pins that canonical shape directly.
    #[cfg(grovedb_test_engine = "rocksdb")]
    #[test]
    fn reconcile_deletes_a_secondary_orphan() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        make_pcit_with(&db, b"pcit", &[b"a", b"b"], gv);
        let pristine_listing = db
            .indexed_count_top_k([TEST_LEAF, b"pcit"].as_ref(), 10, false, None, gv)
            .unwrap()
            .expect("top_k");
        assert_eq!(
            pristine_listing.key_pairs(),
            vec![(1u64, b"a".to_vec()), (1u64, b"b".to_vec())],
            "baseline listing"
        );

        drift_secondary(
            &db,
            &[TEST_LEAF, b"pcit"],
            &[secondary_key(99, b"ghost")],
            &[],
            gv,
        );

        let orphan_issue: Vec<Vec<u8>> = vec![
            TEST_LEAF.to_vec(),
            b"pcit".to_vec(),
            b"__cidx_secondary_orphan__".to_vec(),
            b"ghost".to_vec(),
        ];
        assert!(
            issue_keys(&db, gv).contains(&orphan_issue),
            "the injected row must be reported as a secondary orphan first"
        );
        let drifted_root = db.root_hash(None, gv).unwrap().expect("root hash");

        db.reconcile_indexed_tree_secondaries([TEST_LEAF, b"pcit"].as_ref(), None, gv)
            .unwrap()
            .expect("reconcile removes the orphan");

        assert_clean(&db, gv);
        assert_eq!(
            db.indexed_count_top_k([TEST_LEAF, b"pcit"].as_ref(), 10, false, None, gv)
                .unwrap()
                .expect("top_k"),
            pristine_listing,
            "the ghost row must be gone from the count index"
        );
        let repaired_root = db.root_hash(None, gv).unwrap().expect("root hash");
        assert_ne!(
            repaired_root, drifted_root,
            "dropping the orphan must rebind the parent and move the root hash"
        );
        db.reconcile_indexed_tree_secondaries([TEST_LEAF, b"pcit"].as_ref(), None, gv)
            .unwrap()
            .expect("second reconcile");
        assert_eq!(
            db.root_hash(None, gv).unwrap().expect("root hash"),
            repaired_root,
            "a repaired index must already be a fixed point"
        );
    }

    // -----------------------------------------------------------------
    // Repair: insert rows the primary requires
    // -----------------------------------------------------------------

    /// A primary entry with no secondary row (`__cidx_primary_orphan__`) must be
    /// re-indexed by reconcile at the count the primary actually records.
    #[cfg(grovedb_test_engine = "rocksdb")]
    #[test]
    fn reconcile_reinserts_a_missing_secondary_row() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        make_pcit_with(&db, b"pcit", &[b"a", b"b"], gv);

        drift_secondary(
            &db,
            &[TEST_LEAF, b"pcit"],
            &[],
            &[secondary_key(1, b"a")],
            gv,
        );

        let primary_orphan_issue: Vec<Vec<u8>> = vec![
            TEST_LEAF.to_vec(),
            b"pcit".to_vec(),
            b"__cidx_primary_orphan__".to_vec(),
            b"a".to_vec(),
        ];
        assert!(
            issue_keys(&db, gv).contains(&primary_orphan_issue),
            "deleting the row for 'a' must be reported as a primary orphan first"
        );
        assert_eq!(
            db.indexed_count_top_k([TEST_LEAF, b"pcit"].as_ref(), 10, false, None, gv)
                .unwrap()
                .expect("top_k")
                .key_pairs(),
            vec![(1u64, b"b".to_vec())],
            "the drifted index must be missing 'a'"
        );
        let drifted_root = db.root_hash(None, gv).unwrap().expect("root hash");

        db.reconcile_indexed_tree_secondaries([TEST_LEAF, b"pcit"].as_ref(), None, gv)
            .unwrap()
            .expect("reconcile reinserts the missing row");

        assert_clean(&db, gv);
        assert_eq!(
            db.indexed_count_top_k([TEST_LEAF, b"pcit"].as_ref(), 10, false, None, gv)
                .unwrap()
                .expect("top_k")
                .key_pairs(),
            vec![(1u64, b"a".to_vec()), (1u64, b"b".to_vec())],
            "'a' must be back in the count index at count 1"
        );
        assert_ne!(
            db.root_hash(None, gv).unwrap().expect("root hash"),
            drifted_root,
            "re-indexing 'a' must rebind the parent and move the root hash"
        );
    }

    /// Both repair directions in one pass, against a non-trivial count: the row
    /// for `a` is at the wrong count, so reconcile must delete `(7, "a")` *and*
    /// insert `(1, "a")`. Pins that the deletion set and the insertion set are
    /// computed independently rather than one being derived from the other.
    #[cfg(grovedb_test_engine = "rocksdb")]
    #[test]
    fn reconcile_moves_a_row_that_sits_at_the_wrong_count() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        make_pcit_with(&db, b"pcit", &[b"a", b"b"], gv);

        drift_secondary(
            &db,
            &[TEST_LEAF, b"pcit"],
            &[secondary_key(7, b"a")],
            &[secondary_key(1, b"a")],
            gv,
        );

        let mismatch_issue: Vec<Vec<u8>> = vec![
            TEST_LEAF.to_vec(),
            b"pcit".to_vec(),
            b"__cidx_count_mismatch__".to_vec(),
            b"a".to_vec(),
        ];
        assert!(
            issue_keys(&db, gv).contains(&mismatch_issue),
            "moving 'a' to count 7 must be reported as a count mismatch first"
        );
        assert_eq!(
            db.indexed_count_top_k([TEST_LEAF, b"pcit"].as_ref(), 10, true, None, gv)
                .unwrap()
                .expect("top_k")
                .key_pairs(),
            vec![(7u64, b"a".to_vec()), (1u64, b"b".to_vec())],
            "the drifted index must rank 'a' at 7"
        );
        let drifted_root = db.root_hash(None, gv).unwrap().expect("root hash");

        db.reconcile_indexed_tree_secondaries([TEST_LEAF, b"pcit"].as_ref(), None, gv)
            .unwrap()
            .expect("reconcile moves the row back");

        assert_clean(&db, gv);
        assert_eq!(
            db.indexed_count_top_k([TEST_LEAF, b"pcit"].as_ref(), 10, false, None, gv)
                .unwrap()
                .expect("top_k")
                .key_pairs(),
            vec![(1u64, b"a".to_vec()), (1u64, b"b".to_vec())],
            "'a' must be back at count 1"
        );
        assert_ne!(
            db.root_hash(None, gv).unwrap().expect("root hash"),
            drifted_root,
            "moving the row must rebind the parent and move the root hash"
        );
    }

    /// The rebuild replays the desired keyset from a `BTreeSet`, so the repaired
    /// secondary's AVL shape — and therefore the `combine_hash_three` parent
    /// binding and the GroveDB root hash — depends only on the primary's
    /// contents, never on how the index got out of sync.
    ///
    /// This is the contract that lets two operators repair identical databases
    /// and end up with identical bytes; a hashed iteration order in step 6 would
    /// break it. Three databases with the same primary but three unrelated kinds
    /// of drift must reconcile to one root hash.
    #[cfg(grovedb_test_engine = "rocksdb")]
    #[test]
    fn reconcile_is_shape_deterministic_regardless_of_how_the_index_drifted() {
        let gv = GroveVersion::latest();

        let with_orphan = make_test_grovedb(gv);
        make_pcit_with(&with_orphan, b"pcit", &[b"a", b"b"], gv);
        drift_secondary(
            &with_orphan,
            &[TEST_LEAF, b"pcit"],
            &[secondary_key(99, b"ghost")],
            &[],
            gv,
        );

        let with_missing_row = make_test_grovedb(gv);
        make_pcit_with(&with_missing_row, b"pcit", &[b"a", b"b"], gv);
        drift_secondary(
            &with_missing_row,
            &[TEST_LEAF, b"pcit"],
            &[],
            &[secondary_key(1, b"a")],
            gv,
        );

        let with_wrong_count = make_test_grovedb(gv);
        make_pcit_with(&with_wrong_count, b"pcit", &[b"a", b"b"], gv);
        drift_secondary(
            &with_wrong_count,
            &[TEST_LEAF, b"pcit"],
            &[secondary_key(7, b"a")],
            &[secondary_key(1, b"a")],
            gv,
        );

        let mut roots = Vec::new();
        for db in [&with_orphan, &with_missing_row, &with_wrong_count] {
            db.reconcile_indexed_tree_secondaries([TEST_LEAF, b"pcit"].as_ref(), None, gv)
                .unwrap()
                .expect("reconcile");
            assert_clean(db, gv);
            roots.push(db.root_hash(None, gv).unwrap().expect("root hash"));
        }

        assert_eq!(
            roots[0], roots[1],
            "an orphaned row and a missing row must repair to the same canonical shape"
        );
        assert_eq!(
            roots[1], roots[2],
            "a missing row and a wrongly counted row must repair to the same canonical shape"
        );
    }

    /// Reconcile must derive counts from the primary's *aggregates*, not assume
    /// every entry contributes 1. A PCIT whose children are `ProvableCountTree`
    /// subtrees carries their descendant counts, and the repaired secondary must
    /// rank by those.
    #[cfg(grovedb_test_engine = "rocksdb")]
    #[test]
    fn reconcile_uses_the_primary_aggregate_count_for_each_row() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcit",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("create PCIT");
        for child in [b"big".as_ref(), b"small".as_ref()] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"pcit"].as_ref(),
                child,
                Element::empty_provable_count_tree(),
                None,
                gv,
            )
            .unwrap()
            .expect("insert child count tree");
        }
        // Populate the children through the generic path — legal because the
        // target merk is the child ProvableCountTree, not the indexed primary.
        for i in 0..3u8 {
            db.insert(
                [TEST_LEAF, b"pcit", b"big"].as_ref(),
                &[i],
                Element::new_item(vec![i]),
                None,
                None,
                gv,
            )
            .unwrap()
            .expect("populate big");
        }
        db.insert(
            [TEST_LEAF, b"pcit", b"small"].as_ref(),
            b"only",
            Element::new_item(b"x".to_vec()),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("populate small");

        assert_eq!(
            db.indexed_count_top_k([TEST_LEAF, b"pcit"].as_ref(), 10, true, None, gv)
                .unwrap()
                .expect("top_k")
                .key_pairs(),
            vec![(3u64, b"big".to_vec()), (1u64, b"small".to_vec())],
            "baseline: the children rank by their own descendant counts"
        );

        // Drop the row for the multi-count child, then rebuild it.
        drift_secondary(
            &db,
            &[TEST_LEAF, b"pcit"],
            &[],
            &[secondary_key(3, b"big")],
            gv,
        );
        assert_eq!(
            db.indexed_count_top_k([TEST_LEAF, b"pcit"].as_ref(), 10, true, None, gv)
                .unwrap()
                .expect("top_k")
                .key_pairs(),
            vec![(1u64, b"small".to_vec())],
            "the drifted index must be missing 'big'"
        );

        db.reconcile_indexed_tree_secondaries([TEST_LEAF, b"pcit"].as_ref(), None, gv)
            .unwrap()
            .expect("reconcile rebuilds from the primary");

        assert_eq!(
            db.indexed_count_top_k([TEST_LEAF, b"pcit"].as_ref(), 10, true, None, gv)
                .unwrap()
                .expect("top_k")
                .key_pairs(),
            vec![(3u64, b"big".to_vec()), (1u64, b"small".to_vec())],
            "reconcile must index each child at its own aggregate count, not at 1"
        );
        assert_clean(&db, gv);
    }

    // -----------------------------------------------------------------
    // Refusals
    // -----------------------------------------------------------------

    #[test]
    fn reconcile_rejects_a_path_that_is_not_a_count_indexed_tree() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"plain",
            Element::empty_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("create plain tree");

        let err = db
            .reconcile_indexed_tree_secondaries([TEST_LEAF, b"plain"].as_ref(), None, gv)
            .unwrap()
            .expect_err("a plain tree is not reconcilable");
        match err {
            Error::InvalidPath(message) => assert_eq!(
                message,
                "reconcile_indexed_tree_secondaries requires the path's last segment to be a \
                 ProvableCountIndexedTree, ProvableSumIndexedTree or \
                 ProvableCountProvableSumIndexedTree element"
            ),
            other => panic!("expected InvalidPath, got {other:?}"),
        }
    }

    #[test]
    fn reconcile_rejects_the_root_path() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        let err = db
            .reconcile_indexed_tree_secondaries(EMPTY_PATH, None, gv)
            .unwrap()
            .expect_err("the root has no parent to rebind");
        match err {
            Error::InvalidPath(message) => {
                assert_eq!(message, "cannot reconcile an indexed tree at the root path")
            }
            other => panic!("expected InvalidPath, got {other:?}"),
        }
    }

    /// A primary key longer than the 247-byte cidx ceiling cannot be mirrored:
    /// `sort_key ‖ item_key` would exceed Merk's 256-byte limit. Every write
    /// door refuses such a key, so reaching it means the primary was written by
    /// something that bypassed the check — reconcile must fail closed rather
    /// than synthesize an oversize secondary key.
    #[cfg(grovedb_test_engine = "rocksdb")]
    #[test]
    fn reconcile_refuses_a_primary_key_over_the_cidx_ceiling() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcit",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("create empty PCIT");

        // Both write doors refuse the key, which is what makes this state
        // unreachable through the API and the check a fail-closed guard.
        let oversize = vec![b'k'; 248];
        let via_api = db
            .insert_into_count_indexed_tree(
                [TEST_LEAF, b"pcit"].as_ref(),
                &oversize,
                Element::new_item(b"v".to_vec()),
                None,
                gv,
            )
            .unwrap()
            .expect_err("248-byte key must be refused by the dedicated door");
        assert!(
            matches!(via_api, Error::InvalidInput(_)),
            "expected InvalidInput, got {via_api:?}"
        );

        // Inject it straight into the primary Merk, simulating the corrupt
        // state the guard exists for.
        {
            let tx = db.start_transaction();
            let batch = StorageBatch::new();
            let path_segments: [&[u8]; 2] = [TEST_LEAF, b"pcit"];
            let path: SubtreePath<&[u8]> = (&path_segments).into();
            {
                let mut primary = db
                    .open_transactional_merk_at_path(path, &tx, Some(&batch), gv)
                    .unwrap()
                    .expect("open PCIT primary");
                Element::new_item(b"v".to_vec())
                    .insert(&mut primary, oversize.as_slice(), None, gv)
                    .unwrap()
                    .expect("raw primary insert");
            }
            db.db
                .storage()
                .commit_multi_context_batch(batch, Some(&tx))
                .unwrap()
                .expect("commit");
            tx.commit().expect("tx commit");
        }

        let err = db
            .reconcile_indexed_tree_secondaries([TEST_LEAF, b"pcit"].as_ref(), None, gv)
            .unwrap()
            .expect_err("reconcile must refuse an oversize primary key");
        match err {
            Error::CorruptedData(message) => {
                assert!(
                    message.starts_with(
                        "reconcile_indexed_tree_secondaries found a primary key of length \
                         248 bytes which exceeds this tree's per-axis ceiling of 247 bytes"
                    ),
                    "unexpected message: {message}"
                );
            }
            other => panic!("expected CorruptedData, got {other:?}"),
        }
    }
    /// The secondary transitions to empty and back. When the last row goes the
    /// parent records `secondary_root_key = None`, so the next write reopens the
    /// secondary as a base merk over storage that still holds the old tree's
    /// nodes. The repopulated index must contain only the new entry — a resurrected
    /// row would show up here and in `verify_grovedb`.
    #[test]
    fn emptying_the_index_and_repopulating_it_does_not_resurrect_the_old_row() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        make_pcit_with(&db, b"pcit", &[b"a"], gv);
        assert_clean(&db, gv);
        assert!(db
            .delete_from_count_indexed_tree([TEST_LEAF, b"pcit"].as_ref(), b"a", None, gv)
            .unwrap()
            .expect("delete"));
        assert_clean(&db, gv);
        assert!(
            db.indexed_count_top_k([TEST_LEAF, b"pcit"].as_ref(), 10, false, None, gv)
                .unwrap()
                .expect("top_k")
                .is_empty(),
            "index must be empty after deleting the only entry"
        );
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"pcit"].as_ref(),
            b"z",
            Element::new_item(b"z".to_vec()),
            None,
            gv,
        )
        .unwrap()
        .expect("reinsert");
        assert_eq!(
            db.indexed_count_top_k([TEST_LEAF, b"pcit"].as_ref(), 10, false, None, gv)
                .unwrap()
                .expect("top_k")
                .key_pairs(),
            vec![(1u64, b"z".to_vec())],
            "only 'z' must be indexed"
        );
        assert_clean(&db, gv);
    }

    // -----------------------------------------------------------------
    // Malformed secondary rows must surface, not be silently dropped
    // -----------------------------------------------------------------

    #[cfg(grovedb_test_engine = "rocksdb")]
    fn assert_short_key_corruption(err: Error, context: &str) {
        match err {
            Error::CorruptedData(message) => assert!(
                message.contains("axis Count") && message.contains("shorter than 8 bytes"),
                "{context}: unexpected message: {message}"
            ),
            other => panic!("{context}: expected CorruptedData, got {other:?}"),
        }
    }

    /// A secondary row whose key is shorter than the axis's 8-byte sort prefix
    /// carries no recoverable `(count, original_key)` pair. Every place that
    /// decodes a row must raise `CorruptedData` naming the axis and the
    /// expected width — dropping the row instead would let a short-key row
    /// silently shrink a top-k page or a range listing while the caller sees a
    /// successful result.
    ///
    /// The decode sites driven here: the plain top-k walk, the paginated
    /// collect at `offset == 0`, and the range walk. A paginated read with
    /// `offset > 0` skips by counted descent and decodes only *returned* rows,
    /// so its skip phase deliberately does not error — that contract is
    /// asserted here too.
    #[cfg(grovedb_test_engine = "rocksdb")]
    #[test]
    fn a_short_secondary_key_is_reported_by_every_shape_that_decodes_it() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        make_pcit_with(&db, b"pcit", &[b"a", b"b"], gv);

        // 0xff-prefixed so it sorts last ascending / first descending, which is
        // what lets the descending walks reach it before any well-formed row.
        let short_key = vec![0xffu8, 0xff, 0xff];
        drift_secondary(&db, &[TEST_LEAF, b"pcit"], &[short_key], &[], gv);
        assert!(
            issue_keys(&db, gv).iter().any(|k| k
                .iter()
                .any(|seg| seg == b"__cidx_secondary_malformed_key__")),
            "verify_grovedb must flag the malformed row: {:?}",
            issue_keys(&db, gv)
        );

        assert_short_key_corruption(
            db.indexed_count_top_k([TEST_LEAF, b"pcit"].as_ref(), 5, true, None, gv)
                .unwrap()
                .expect_err("top_k must surface the malformed row"),
            "top_k",
        );
        assert_short_key_corruption(
            db.indexed_count_top_k_paginated([TEST_LEAF, b"pcit"].as_ref(), 5, 0, true, None, gv)
                .unwrap()
                .expect_err("paginated collect must surface the malformed row"),
            "paginated (offset 0, collect loop)",
        );
        // A paginated read with `offset > 0` skips by counted descent over
        // the secondary merk: skipped rows are consumed from aggregate
        // counts and their keys are never decoded. The malformed row is a
        // real tree row here, so it still occupies its position (descending
        // order is [0xff…, b, a] and offset 1 lands on b — the same
        // position the storage iterator would assign), but only *returned*
        // rows are decoded, so the skip no longer surfaces it as an error.
        // Detecting malformed rows in the skipped region is
        // `verify_grovedb`'s job (asserted above) and the collect loops'
        // (asserted below for every shape that reads the row itself).
        assert_eq!(
            db.indexed_count_top_k_paginated([TEST_LEAF, b"pcit"].as_ref(), 1, 1, true, None, gv)
                .unwrap()
                .expect("counted skip passes the malformed row without decoding it")
                .entries
                .key_pairs(),
            vec![(1u64, b"b".to_vec())],
            "descending order is [0xff…, b, a]; the malformed row is counted at offset 0",
        );
        assert_short_key_corruption(
            db.indexed_count_range(
                [TEST_LEAF, b"pcit"].as_ref(),
                0,
                u64::MAX,
                false,
                10,
                None,
                gv,
            )
            .unwrap()
            .expect_err("range must surface the malformed row"),
            "range",
        );

        // When the malformed row is the RETURNED position of a counted read
        // (ascending order is [a, b, 0xff…]; offset 2 selects it), the
        // counted path must decode-and-refuse it exactly as the iterator
        // paths do — skipped rows go undecoded, returned rows never do.
        assert_short_key_corruption(
            db.indexed_count_top_k_paginated([TEST_LEAF, b"pcit"].as_ref(), 1, 2, false, None, gv)
                .unwrap()
                .expect_err("a returned malformed row must surface through the counted path"),
            "paginated (offset 2, counted path, returned row)",
        );

        // Ascending top-k stops before reaching the malformed row, so the
        // well-formed prefix of the index still reads cleanly — the error above
        // is the decoder refusing a specific row, not the query failing wholesale.
        assert_eq!(
            db.indexed_count_top_k([TEST_LEAF, b"pcit"].as_ref(), 2, false, None, gv)
                .unwrap()
                .expect("the first two rows are well formed")
                .key_pairs(),
            vec![(1u64, b"a".to_vec()), (1u64, b"b".to_vec())],
        );
    }

    // -----------------------------------------------------------------
    // Path validation on the per-axis query APIs
    // -----------------------------------------------------------------

    /// The root has no parent element to read a secondary root key from, so an
    /// axis query there is rejected before any merk is opened.
    #[test]
    fn a_per_axis_query_at_the_root_path_is_rejected() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        let err = db
            .indexed_count_top_k(EMPTY_PATH, 5, false, None, gv)
            .unwrap()
            .expect_err("the root path names no indexed tree");
        match err {
            Error::InvalidPath(message) => {
                assert_eq!(message, "cannot query an indexed tree at the root path")
            }
            other => panic!("expected InvalidPath, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // The dedicated insert door only accepts EMPTY tree/indexed children
    // -----------------------------------------------------------------

    fn assert_rejects_non_empty_child(child: Element, label: &str) {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcit",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("create PCIT");
        let root_before = db.root_hash(None, gv).unwrap().expect("root hash");

        let err = db
            .insert_into_count_indexed_tree(
                [TEST_LEAF, b"pcit"].as_ref(),
                b"child",
                child,
                None,
                gv,
            )
            .unwrap()
            .unwrap_err();
        match err {
            Error::NotSupported(message) => assert!(
                message.starts_with(
                    "insert_into_count_indexed_tree only accepts EMPTY tree/indexed child elements"
                ),
                "{label}: unexpected message: {message}"
            ),
            other => panic!("{label}: expected NotSupported, got {other:?}"),
        }
        assert_eq!(
            db.root_hash(None, gv).unwrap().expect("root hash"),
            root_before,
            "{label}: a rejected insert must not touch state"
        );
        assert_clean(&db, gv);
    }

    /// A rootless `BigSumTree` claiming a non-zero sum has no contents the sum
    /// could have been derived from, so the value is a bare assertion. The
    /// dedicated door creates children empty and would bind this one to an
    /// empty merk node, persisting an element whose stored aggregate disagrees
    /// with what it points at.
    #[test]
    fn the_dedicated_insert_refuses_a_rootless_big_sum_tree_child_with_a_claimed_sum() {
        assert_rejects_non_empty_child(Element::BigSumTree(None, 5, None), "BigSumTree(None, 5)");
    }

    /// Same rule for a nested indexed child: a PCPSIT whose axes TLV already
    /// names a secondary root key is describing on-disk secondary state the
    /// dedicated path never built or validated.
    #[test]
    fn the_dedicated_insert_refuses_a_pcpsit_child_claiming_a_populated_axis() {
        assert_rejects_non_empty_child(
            Element::ProvableCountProvableSumIndexedTree(
                None,
                0,
                0,
                vec![(IndexAxis::Count.tag(), Some(b"forged".to_vec()))],
                None,
            ),
            "PCPSIT with a populated axis",
        );
    }

    // -----------------------------------------------------------------
    // Per-variant repair: PSIT and PCPSIT
    // -----------------------------------------------------------------

    use crate::operations::indexed_tree::{axis_secondary_tree_type, make_axis_secondary_key};

    fn make_psit_with(db: &TempGroveDb, key: &[u8], sums: &[(&[u8], i64)], gv: &GroveVersion) {
        db.insert(
            [TEST_LEAF].as_ref(),
            key,
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("create PSIT");
        for (child, sum) in sums {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, key].as_ref(),
                child,
                Element::new_sum_item(*sum),
                None,
                gv,
            )
            .unwrap()
            .expect("populate PSIT");
        }
    }

    fn make_pcpsit_with(db: &TempGroveDb, key: &[u8], rows: &[(&[u8], i64)], gv: &GroveVersion) {
        db.insert(
            [TEST_LEAF].as_ref(),
            key,
            Element::empty_provable_count_provable_sum_indexed_tree(vec![
                (0u8, None),
                (1u8, None),
                (2u8, None),
            ])
            .expect("canonical axes"),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("create PCPSIT");
        for (child, sum) in rows {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, key].as_ref(),
                child,
                Element::new_item_with_sum_item(b"v".to_vec(), *sum),
                None,
                gv,
            )
            .unwrap()
            .expect("populate PCPSIT");
        }
    }

    /// Axis-aware sibling of [`drift_secondary`]: edit rows in ONE axis
    /// secondary of any indexed variant, then rebind the parent element to
    /// every axis's current `(root_hash, root_key)` so the damage is a
    /// *coherent* wrong index rather than a torn one — the same reasoning as
    /// the PCIT helper's doc.
    ///
    /// `overwrite` entries write a payload at a key IN PLACE (no structural
    /// change — an existing key keeps its node position); `insert` adds new
    /// rows and `delete` removes rows, both of which can rotate the AVL and
    /// therefore permanently change tree SHAPE (see the shape caveat on
    /// `reconcile_indexed_tree_secondaries`).
    fn drift_axis_secondary(
        db: &TempGroveDb,
        indexed_path: &[&[u8]],
        axis: IndexAxis,
        overwrite_or_insert: &[(Vec<u8>, Element)],
        delete: &[Vec<u8>],
        gv: &GroveVersion,
    ) {
        use grovedb_merk::element::reconstruct::ElementReconstructExtensions;

        let tx = db.start_transaction();
        let batch = SuiteBatch::default();
        let owned: Vec<&[u8]> = indexed_path.to_vec();
        let path: SubtreePath<&[u8]> = owned.as_slice().into();
        let (parent_path, indexed_key) = path.derive_parent().expect("non-root indexed tree");

        let (primary_root_hash, primary_root_key, primary_aggregate) = {
            let primary = db
                .open_transactional_merk_at_path(path.clone(), &tx, Some(&batch), gv)
                .unwrap()
                .expect("open primary");
            primary
                .root_hash_key_and_aggregate_data()
                .unwrap()
                .expect("primary root state")
        };

        let mut parent_merk = db
            .open_transactional_merk_at_path(parent_path.clone(), &tx, Some(&batch), gv)
            .unwrap()
            .expect("open parent merk");
        let indexed_element = Element::get(&parent_merk, indexed_key, true, gv)
            .unwrap()
            .expect("indexed element");
        let axes: Vec<(IndexAxis, Option<Vec<u8>>)> = match indexed_element.underlying() {
            Element::ProvableCountIndexedTree(_, s, ..) => vec![(IndexAxis::Count, s.clone())],
            Element::ProvableSumIndexedTree(_, s, ..) => vec![(IndexAxis::Sum, s.clone())],
            Element::ProvableCountProvableSumIndexedTree(_, _, _, tlv, _) => tlv
                .iter()
                .map(|(tag, root_key)| {
                    (
                        IndexAxis::try_from_tag(*tag).expect("canonical tag"),
                        root_key.clone(),
                    )
                })
                .collect(),
            other => panic!("not an indexed element: {other:?}"),
        };

        // Apply the edits to the chosen axis; read every axis's post-state.
        let mut per_axis: Vec<(u8, grovedb_merk::CryptoHash, Option<Vec<u8>>)> = Vec::new();
        for (candidate, stored_root_key) in &axes {
            let mut secondary = db
                .open_indexed_secondary_at_path(
                    path.clone(),
                    *candidate,
                    stored_root_key.clone(),
                    &tx,
                    Some(&batch),
                    gv,
                )
                .unwrap()
                .expect("open axis secondary");
            if *candidate == axis {
                for key in delete {
                    Element::delete(
                        &mut secondary,
                        key.as_slice(),
                        None,
                        false,
                        axis_secondary_tree_type(axis),
                        gv,
                    )
                    .unwrap()
                    .expect("delete secondary row");
                }
                for (key, element) in overwrite_or_insert {
                    element
                        .clone()
                        .insert(&mut secondary, key.as_slice(), None, gv)
                        .unwrap()
                        .expect("write secondary row");
                }
            }
            let (hash, root_key, _) = secondary
                .root_hash_key_and_aggregate_data()
                .unwrap()
                .expect("axis root state");
            per_axis.push((candidate.tag(), hash, root_key));
        }

        // Rebind the parent element per variant.
        let is_multi_axis = matches!(
            indexed_element.underlying(),
            Element::ProvableCountProvableSumIndexedTree(..)
        );
        let (rebound, second_hash) = if is_multi_axis {
            let tlv: Vec<(u8, Option<Vec<u8>>)> = per_axis
                .iter()
                .map(|(tag, _, root_key)| (*tag, root_key.clone()))
                .collect();
            let hashes: Vec<(u8, grovedb_merk::CryptoHash)> = per_axis
                .iter()
                .map(|(tag, hash, _)| (*tag, *hash))
                .collect();
            let digest = grovedb_merk::tree::axes_digest(&hashes).unwrap();
            (
                indexed_element
                    .reconstruct_with_axes(primary_root_key, primary_aggregate, tlv)
                    .expect("reconstruct PCPSIT"),
                digest,
            )
        } else {
            (
                indexed_element
                    .reconstruct_with_two_root_keys(
                        primary_root_key,
                        per_axis[0].2.clone(),
                        primary_aggregate,
                    )
                    .expect("reconstruct single-axis element"),
                per_axis[0].1,
            )
        };
        rebound
            .insert_count_indexed_subtree(
                &mut parent_merk,
                indexed_key,
                primary_root_hash,
                second_hash,
                None,
                gv,
            )
            .unwrap()
            .expect("rebind indexed element");

        let mut merk_cache = std::collections::HashMap::new();
        merk_cache.insert(parent_path.clone(), parent_merk);
        db.propagate_changes_with_transaction(merk_cache, parent_path, &tx, &batch, gv)
            .unwrap()
            .expect("propagate rebind");
        db.db
            .apply_batch(batch, &tx)
            .unwrap()
            .expect("commit axis drift");
        db.commit_transaction(tx).unwrap().expect("tx commit");
    }

    /// Rebind a PCIT's parent element so its `secondary_root_key` names a
    /// key that does not exist in the count secondary, leaving every row
    /// and hash otherwise intact. Produces the state a counted read must
    /// treat as corruption: the element is readable and valid, but the
    /// root it names resolves to nothing.
    #[cfg(grovedb_test_engine = "rocksdb")]
    fn rebind_count_secondary_root_key_to(
        db: &TempGroveDb,
        pcit_path: &[&[u8]],
        bogus_key: Vec<u8>,
        gv: &GroveVersion,
    ) {
        use grovedb_merk::element::reconstruct::ElementReconstructExtensions;

        let tx = db.start_transaction();
        let batch = StorageBatch::new();
        let owned: Vec<&[u8]> = pcit_path.to_vec();
        let path: SubtreePath<&[u8]> = owned.as_slice().into();
        let (parent_path, pcit_key) = path.derive_parent().expect("non-root PCIT");

        let (primary_root_hash, primary_root_key, primary_aggregate) = {
            let primary = db
                .open_transactional_merk_at_path(path.clone(), &tx, Some(&batch), gv)
                .unwrap()
                .expect("open PCIT primary");
            primary
                .root_hash_key_and_aggregate_data()
                .unwrap()
                .expect("primary root state")
        };

        let mut parent_merk = db
            .open_transactional_merk_at_path(parent_path.clone(), &tx, Some(&batch), gv)
            .unwrap()
            .expect("open parent merk");
        let pcit_element = Element::get(&parent_merk, pcit_key, true, gv)
            .unwrap()
            .expect("PCIT element");
        let secondary_root_key = match pcit_element.underlying() {
            Element::ProvableCountIndexedTree(_, s, ..) => s.clone(),
            other => panic!("not a PCIT element: {other:?}"),
        };
        let secondary_root_hash = {
            let secondary = db
                .open_indexed_secondary_at_path(
                    path.clone(),
                    IndexAxis::Count,
                    secondary_root_key,
                    &tx,
                    Some(&batch),
                    gv,
                )
                .unwrap()
                .expect("open count secondary");
            let (hash, _, _) = secondary
                .root_hash_key_and_aggregate_data()
                .unwrap()
                .expect("secondary root state");
            hash
        };

        let rebound = pcit_element
            .reconstruct_with_two_root_keys(primary_root_key, Some(bogus_key), primary_aggregate)
            .expect("reconstruct PCIT element");
        rebound
            .insert_count_indexed_subtree(
                &mut parent_merk,
                pcit_key,
                primary_root_hash,
                secondary_root_hash,
                None,
                gv,
            )
            .unwrap()
            .expect("rebind PCIT element");

        let mut merk_cache = std::collections::HashMap::new();
        merk_cache.insert(parent_path.clone(), parent_merk);
        db.propagate_changes_with_transaction(merk_cache, parent_path, &tx, &batch, gv)
            .unwrap()
            .expect("propagate rebind");
        db.db
            .storage()
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .expect("commit rebind");
        tx.commit().expect("tx commit");
    }

    /// A parent element whose `secondary_root_key` names a node that does
    /// not exist must fail loud through the counted path. The pinned view
    /// read the element itself, so a dangling root key is corruption, not
    /// a race to retry through — and it must never be silently served as
    /// an empty or truncated page. The offset-0 iterator path, which
    /// never consults the root key, still reads the physical rows: the
    /// contrast is the two-views posture the design doc records.
    #[cfg(grovedb_test_engine = "rocksdb")]
    #[test]
    fn a_dangling_secondary_root_key_fails_loud_through_the_counted_path() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        make_pcit_with(&db, b"pcit", &[b"a", b"b"], gv);
        rebind_count_secondary_root_key_to(
            &db,
            &[TEST_LEAF, b"pcit"],
            b"no-such-node".to_vec(),
            gv,
        );

        let err = db
            .indexed_count_top_k_paginated([TEST_LEAF, b"pcit"].as_ref(), 1, 1, false, None, gv)
            .unwrap()
            .expect_err("a dangling secondary root key must error, not serve a page");
        match err {
            Error::CorruptedData(message) => assert!(
                message.contains("absent from the same read snapshot"),
                "unexpected corruption message: {message}"
            ),
            other => panic!("expected CorruptedData, got {other:?}"),
        }

        // The iterator path reads the physical keyspace and is untouched
        // by the dangling root key.
        assert_eq!(
            db.indexed_count_top_k_paginated([TEST_LEAF, b"pcit"].as_ref(), 2, 0, false, None, gv)
                .unwrap()
                .expect("offset-0 path reads physical rows")
                .entries
                .key_pairs(),
            vec![(1u64, b"a".to_vec()), (1u64, b"b".to_vec())],
        );
    }

    /// PSIT: an orphan row in the sum secondary is removed and the root
    /// returns to the pristine twin's — the repair covers the sum axis, not
    /// just count.
    #[cfg(grovedb_test_engine = "rocksdb")]
    #[test]
    fn reconcile_repairs_a_psit_orphan_row() {
        let gv = GroveVersion::latest();
        let rows: &[(&[u8], i64)] = &[(b"a", 7), (b"b", -3)];

        let db = make_test_grovedb(gv);
        make_psit_with(&db, b"psit", rows, gv);
        let pristine = make_test_grovedb(gv);
        make_psit_with(&pristine, b"psit", rows, gv);

        let ghost_key = make_axis_secondary_key(IndexAxis::Sum, 0, 99, b"ghost");
        drift_axis_secondary(
            &db,
            &[TEST_LEAF, b"psit"],
            IndexAxis::Sum,
            &[(ghost_key, Element::new_sum_item(99))],
            &[],
            gv,
        );
        assert_ne!(
            db.root_hash(None, gv).unwrap().unwrap(),
            pristine.root_hash(None, gv).unwrap().unwrap(),
            "pre-condition: the drift must have moved the root"
        );

        db.reconcile_indexed_tree_secondaries([TEST_LEAF, b"psit"].as_ref(), None, gv)
            .unwrap()
            .expect("reconcile PSIT");
        assert_eq!(
            db.root_hash(None, gv).unwrap().unwrap(),
            pristine.root_hash(None, gv).unwrap().unwrap(),
            "the repaired PSIT must be byte-identical to the pristine twin"
        );
        assert_clean(&db, gv);
    }

    /// PCPSIT: a row at the CORRECT avg sort key carrying a damaged payload
    /// must be rewritten. Key-presence alone cannot detect this — it is the
    /// content-compare path the count-only reconcile never needed. The
    /// damage is an IN-PLACE overwrite (no delete), so the tree shape is
    /// untouched and pristine-equality is the correct expectation per the
    /// shape caveat on the repair API.
    #[cfg(grovedb_test_engine = "rocksdb")]
    #[test]
    fn reconcile_rewrites_a_payload_damaged_avg_row() {
        let gv = GroveVersion::latest();
        let rows: &[(&[u8], i64)] = &[(b"a", 10), (b"b", 4)];

        let db = make_test_grovedb(gv);
        make_pcpsit_with(&db, b"idx", rows, gv);
        let pristine = make_test_grovedb(gv);
        make_pcpsit_with(&pristine, b"idx", rows, gv);

        // Each child is (count 1, sum s): its avg row key encodes avg = s.
        // Overwrite b"a"'s payload with a wrong sum at the SAME key, in
        // place.
        let avg_key = make_axis_secondary_key(IndexAxis::Avg, 1, 10, b"a");
        drift_axis_secondary(
            &db,
            &[TEST_LEAF, b"idx"],
            IndexAxis::Avg,
            &[(avg_key, Element::new_item_with_sum_item(Vec::new(), 9999))],
            &[],
            gv,
        );
        assert_ne!(
            db.root_hash(None, gv).unwrap().unwrap(),
            pristine.root_hash(None, gv).unwrap().unwrap(),
            "pre-condition: the payload damage must have moved the root"
        );

        db.reconcile_indexed_tree_secondaries([TEST_LEAF, b"idx"].as_ref(), None, gv)
            .unwrap()
            .expect("reconcile PCPSIT");
        assert_eq!(
            db.root_hash(None, gv).unwrap().unwrap(),
            pristine.root_hash(None, gv).unwrap().unwrap(),
            "the payload-damaged avg row must have been rewritten in place"
        );
        assert_clean(&db, gv);
    }

    /// Two PCPSITs damaged DIFFERENTLY (a ghost row on the sum axis, a
    /// deleted row on the avg axis) must both reconcile back to the pristine
    /// twin's root — canonical content repair across axes, with damage whose
    /// undo restores the previous shape.
    #[cfg(grovedb_test_engine = "rocksdb")]
    #[test]
    fn reconcile_pcpsit_is_canonical_regardless_of_which_axis_drifted() {
        let gv = GroveVersion::latest();
        let rows: &[(&[u8], i64)] = &[(b"a", 5), (b"b", 11), (b"c", -2)];

        let sum_drifted = make_test_grovedb(gv);
        make_pcpsit_with(&sum_drifted, b"idx", rows, gv);
        let avg_drifted = make_test_grovedb(gv);
        make_pcpsit_with(&avg_drifted, b"idx", rows, gv);
        let pristine = make_test_grovedb(gv);
        make_pcpsit_with(&pristine, b"idx", rows, gv);

        drift_axis_secondary(
            &sum_drifted,
            &[TEST_LEAF, b"idx"],
            IndexAxis::Sum,
            &[(
                make_axis_secondary_key(IndexAxis::Sum, 0, 77, b"ghost"),
                Element::new_sum_item(77),
            )],
            &[],
            gv,
        );
        drift_axis_secondary(
            &avg_drifted,
            &[TEST_LEAF, b"idx"],
            IndexAxis::Avg,
            &[],
            &[make_axis_secondary_key(IndexAxis::Avg, 1, 11, b"b")],
            gv,
        );

        for db in [&sum_drifted, &avg_drifted] {
            db.reconcile_indexed_tree_secondaries([TEST_LEAF, b"idx"].as_ref(), None, gv)
                .unwrap()
                .expect("reconcile");
        }
        let pristine_root = pristine.root_hash(None, gv).unwrap().unwrap();
        assert_eq!(
            sum_drifted.root_hash(None, gv).unwrap().unwrap(),
            pristine_root,
            "sum-axis damage must reconcile to the pristine root"
        );
        assert_eq!(
            avg_drifted.root_hash(None, gv).unwrap().unwrap(),
            pristine_root,
            "avg-axis damage must reconcile to the pristine root"
        );
        assert_clean(&sum_drifted, gv);
        assert_clean(&avg_drifted, gv);
    }

    /// Idempotence for the multi-axis variant: reconciling a healthy PCPSIT
    /// must not move the root hash.
    #[test]
    fn reconcile_of_a_healthy_pcpsit_leaves_the_root_hash_untouched() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        make_pcpsit_with(&db, b"idx", &[(b"a", 3), (b"b", 9)], gv);
        let before = db.root_hash(None, gv).unwrap().unwrap();

        db.reconcile_indexed_tree_secondaries([TEST_LEAF, b"idx"].as_ref(), None, gv)
            .unwrap()
            .expect("reconcile healthy PCPSIT");

        assert_eq!(
            db.root_hash(None, gv).unwrap().unwrap(),
            before,
            "a healthy multi-axis reconcile must be a no-op on the root"
        );
        assert_clean(&db, gv);
    }

    // -----------------------------------------------------------------
    // What a repair leaves in storage, and above the repaired tree
    // -----------------------------------------------------------------

    /// The repair as the released versions run it.
    fn as_released() -> GroveVersion {
        let mut version = GroveVersion::latest().clone();
        version
            .grovedb_versions
            .operations
            .indexed_axis
            .reconcile_secondaries = 0;
        version
    }

    /// The records an axis's secondary holds in storage, whether or not its
    /// Merk links them: what the ranking reads iterate.
    fn stored_rows(
        db: &TempGroveDb,
        indexed_path: &[&[u8]],
        axis: IndexAxis,
        gv: &GroveVersion,
    ) -> Vec<(Vec<u8>, Vec<u8>)> {
        use grovedb_merk::{merk::KVIterator, proofs::Query, store::NodeStore};

        let tx = db.start_transaction();
        let path: SubtreePath<&[u8]> = indexed_path.into();
        let secondary = db
            .open_indexed_secondary_at_path(path, axis, None, &tx, None, gv)
            .unwrap()
            .expect("open the secondary's storage");
        let mut all = Query::new();
        all.insert_all();
        let mut rows = KVIterator::new(NodeStore::cursor(&secondary.storage), &all).unwrap();
        let mut stored = Vec::new();
        while let Some(row) = rows.next_kv().unwrap() {
            stored.push(row);
        }
        stored
    }

    fn stored_row_keys(
        db: &TempGroveDb,
        indexed_path: &[&[u8]],
        axis: IndexAxis,
        gv: &GroveVersion,
    ) -> Vec<Vec<u8>> {
        stored_rows(db, indexed_path, axis, gv)
            .into_iter()
            .map(|(key, _)| key)
            .collect()
    }

    /// Writes a record straight into an axis's secondary storage, inside
    /// `tx`, where no Merk links it.
    #[cfg(grovedb_test_engine = "rocksdb")]
    fn store_unlinked_row(
        db: &TempGroveDb,
        indexed_path: &[&[u8]],
        axis: IndexAxis,
        key: &[u8],
        node_bytes: &[u8],
        tx: &crate::Transaction,
        gv: &GroveVersion,
    ) {
        use grovedb_storage::StorageContext;

        let batch = StorageBatch::new();
        {
            let path: SubtreePath<&[u8]> = indexed_path.into();
            let secondary = db
                .open_indexed_secondary_at_path(path, axis, None, tx, Some(&batch), gv)
                .unwrap()
                .expect("open the secondary's storage");
            secondary
                .storage
                .put(key, node_bytes, None, None)
                .unwrap()
                .expect("store the record");
        }
        db.db
            .storage()
            .commit_multi_context_batch(batch, Some(tx))
            .unwrap()
            .expect("commit the record");
    }

    const REAL_ROWS: &[(&[u8], i64)] = &[(b"a", 7), (b"b", -3)];

    fn real_row_keys() -> Vec<Vec<u8>> {
        let mut keys: Vec<Vec<u8>> = REAL_ROWS
            .iter()
            .map(|(key, sum)| make_axis_secondary_key(IndexAxis::Sum, 0, *sum, key))
            .collect();
        keys.sort();
        keys
    }

    /// A sum indexed tree at `[TEST_LEAF, psit]` with six rows no entry
    /// stands behind, spread between and around the two real ones.
    fn psit_with_stray_rows(gv: &GroveVersion) -> TempGroveDb {
        let db = make_test_grovedb(gv);
        make_psit_with(&db, b"psit", REAL_ROWS, gv);
        let strays: Vec<(Vec<u8>, Element)> = [-40i64, -20, 1, 2, 30, 50]
            .iter()
            .map(|sum| {
                (
                    make_axis_secondary_key(IndexAxis::Sum, 0, *sum, b"ghost"),
                    Element::new_sum_item(*sum),
                )
            })
            .collect();
        drift_axis_secondary(&db, &[TEST_LEAF, b"psit"], IndexAxis::Sum, &strays, &[], gv);
        assert_eq!(
            stored_row_keys(&db, &[TEST_LEAF, b"psit"], IndexAxis::Sum, gv).len(),
            8,
            "pre-condition: the six stray rows are stored"
        );
        db
    }

    /// A repair removes its stray rows one Merk commit at a time, and
    /// removing one rewrites the nodes around it. Every removal must reach
    /// storage, where the ranking reads look.
    #[test]
    fn reconcile_removes_every_stray_row_from_storage() {
        let gv = GroveVersion::latest();
        let pristine = make_test_grovedb(gv);
        make_psit_with(&pristine, b"psit", REAL_ROWS, gv);
        let pristine_root = pristine.root_hash(None, gv).unwrap().unwrap();

        let db = psit_with_stray_rows(gv);
        db.reconcile_indexed_tree_secondaries([TEST_LEAF, b"psit"].as_ref(), None, gv)
            .unwrap()
            .expect("reconcile");
        assert_eq!(db.root_hash(None, gv).unwrap().unwrap(), pristine_root);
        assert_eq!(
            stored_row_keys(&db, &[TEST_LEAF, b"psit"], IndexAxis::Sum, gv),
            real_row_keys()
        );
        assert_clean(&db, gv);

        // As released, the Merk is repaired and rows stay behind in storage.
        let released = as_released();
        let db = psit_with_stray_rows(gv);
        db.reconcile_indexed_tree_secondaries([TEST_LEAF, b"psit"].as_ref(), None, &released)
            .unwrap()
            .expect("reconcile as released");
        assert_eq!(db.root_hash(None, gv).unwrap().unwrap(), pristine_root);
        let left_behind = stored_row_keys(&db, &[TEST_LEAF, b"psit"], IndexAxis::Sum, gv);
        assert!(
            left_behind.len() > 2,
            "the released repair leaves removed rows in storage: {left_behind:?}"
        );

        // Those rows are in no Merk any more, and a repair on the current
        // version removes them from storage all the same, without moving
        // the root.
        db.reconcile_indexed_tree_secondaries([TEST_LEAF, b"psit"].as_ref(), None, gv)
            .unwrap()
            .expect("reconcile again");
        assert_eq!(db.root_hash(None, gv).unwrap().unwrap(), pristine_root);
        assert_eq!(
            stored_row_keys(&db, &[TEST_LEAF, b"psit"], IndexAxis::Sum, gv),
            real_row_keys()
        );
        assert_clean(&db, gv);
    }

    /// A tree converted to a 128-bit sum keeps its big sum rows where its
    /// sum rows were. A sum row left in storage by a released repair, linked
    /// by no Merk, must go with the others: the ranking reads would take it
    /// for a big sum row.
    #[test]
    fn a_conversion_removes_the_sum_rows_no_merk_links() {
        let gv = GroveVersion::latest();
        let db = psit_with_stray_rows(gv);
        db.reconcile_indexed_tree_secondaries([TEST_LEAF, b"psit"].as_ref(), None, &as_released())
            .unwrap()
            .expect("reconcile as released");
        assert!(
            stored_row_keys(&db, &[TEST_LEAF, b"psit"], IndexAxis::Sum, gv).len() > 2,
            "pre-condition: rows no Merk links are stored"
        );

        db.convert_to_big_sum_tree([TEST_LEAF].as_ref(), b"psit", None, gv)
            .unwrap()
            .expect("convert");

        let mut big_sum_rows: Vec<Vec<u8>> = REAL_ROWS
            .iter()
            .map(|(key, sum)| {
                make_axis_secondary_key(
                    IndexAxis::BigSum,
                    0,
                    crate::operations::indexed_tree::EntrySum::Wide(i128::from(*sum)),
                    key,
                )
            })
            .collect();
        big_sum_rows.sort();
        assert_eq!(
            stored_row_keys(&db, &[TEST_LEAF, b"psit"], IndexAxis::BigSum, gv),
            big_sum_rows
        );
        let page = db
            .indexed_big_sum_top_k_paginated([TEST_LEAF, b"psit"].as_ref(), 10, 0, true, None, gv)
            .unwrap()
            .expect("the ranking reads");
        assert_eq!(page.entries.len(), 2);
        assert_clean(&db, gv);
    }

    /// When the repaired tree is an entry of another indexed tree, that
    /// tree's row for it binds the element's value hash, which a repair that
    /// changes a secondary changes.
    #[test]
    fn reconcile_of_a_nested_tree_refreshes_the_row_above_it() {
        let gv = GroveVersion::latest();
        let outer: &[&[u8]] = &[TEST_LEAF, b"outer"];
        let inner: &[&[u8]] = &[TEST_LEAF, b"outer", b"inner"];
        let build = || {
            let db = make_test_grovedb(gv);
            make_psit_with(&db, b"outer", &[(b"x", 5)], gv);
            db.insert_into_provable_sum_indexed_tree(
                outer,
                b"inner",
                Element::empty_provable_sum_indexed_tree(),
                None,
                gv,
            )
            .unwrap()
            .expect("insert the inner tree");
            for (key, sum) in REAL_ROWS {
                db.insert_into_provable_sum_indexed_tree(
                    inner,
                    key,
                    Element::new_sum_item(*sum),
                    None,
                    gv,
                )
                .unwrap()
                .expect("insert an inner entry");
            }
            db
        };
        let pristine_root = build().root_hash(None, gv).unwrap().unwrap();

        // Damage the inner tree's secondary, then repair the outer tree so
        // that its row binds the damaged inner element.
        let damaged = |version: &GroveVersion| {
            let db = build();
            drift_axis_secondary(
                &db,
                inner,
                IndexAxis::Sum,
                &[(
                    make_axis_secondary_key(IndexAxis::Sum, 0, 99, b"ghost"),
                    Element::new_sum_item(99),
                )],
                &[],
                gv,
            );
            db.reconcile_indexed_tree_secondaries(outer, None, gv)
                .unwrap()
                .expect("repair the outer tree");
            assert_ne!(db.root_hash(None, gv).unwrap().unwrap(), pristine_root);

            db.reconcile_indexed_tree_secondaries(inner, None, version)
                .unwrap()
                .expect("repair the inner tree");
            db
        };

        // The repair of the inner tree carries through to the outer row.
        let db = damaged(gv);
        assert_eq!(db.root_hash(None, gv).unwrap().unwrap(), pristine_root);
        assert_clean(&db, gv);

        // As released, the outer row keeps the hash of the damaged element.
        let db = damaged(&as_released());
        assert_ne!(db.root_hash(None, gv).unwrap().unwrap(), pristine_root);
        db.reconcile_indexed_tree_secondaries(outer, None, gv)
            .unwrap()
            .expect("repair the outer tree again");
        assert_eq!(db.root_hash(None, gv).unwrap().unwrap(), pristine_root);
    }

    /// A row can be in storage, exactly as the primary calls for it, and in
    /// no tree: a read by key finds it and its bytes match. The repair must
    /// treat it as the missing row it is to the tree, or the ranking reads
    /// return a row the root hash and the proofs do not have.
    #[cfg(grovedb_test_engine = "rocksdb")]
    #[test]
    fn reconcile_links_a_row_that_is_only_in_storage() {
        let gv = GroveVersion::latest();
        let psit: &[&[u8]] = &[TEST_LEAF, b"psit"];
        // Enough rows that the secondary's root has children: a read by key
        // goes to storage where it meets a link it has not loaded, and
        // finds a record there whether or not the link leads to it.
        let rows: &[(&[u8], i64)] = &[
            (b"a", 7),
            (b"b", -3),
            (b"c", 12),
            (b"d", 40),
            (b"e", -25),
            (b"f", 3),
            (b"g", 90),
        ];
        let row_key = make_axis_secondary_key(IndexAxis::Sum, 0, 7, b"a");

        // The row is taken out of the secondary and, for `record_kept`, its
        // record is put back in storage alone.
        let damaged = |record_kept: bool| {
            let db = make_test_grovedb(gv);
            make_psit_with(&db, b"psit", rows, gv);
            let record = stored_rows(&db, psit, IndexAxis::Sum, gv)
                .into_iter()
                .find(|(key, _)| *key == row_key)
                .expect("the row is stored")
                .1;
            drift_axis_secondary(
                &db,
                psit,
                IndexAxis::Sum,
                &[],
                std::slice::from_ref(&row_key),
                gv,
            );
            if record_kept {
                let tx = db.start_transaction();
                store_unlinked_row(&db, psit, IndexAxis::Sum, &row_key, &record, &tx, gv);
                db.commit_transaction(tx).unwrap().expect("commit");
                assert_eq!(
                    stored_row_keys(&db, psit, IndexAxis::Sum, gv).len(),
                    rows.len(),
                    "pre-condition: every row is in storage"
                );
            }
            db
        };

        // What the repair makes of a secondary the row is simply missing
        // from.
        let missing = damaged(false);
        let damaged_root = missing.root_hash(None, gv).unwrap().unwrap();
        missing
            .reconcile_indexed_tree_secondaries(psit, None, gv)
            .unwrap()
            .expect("reconcile");
        let repaired_root = missing.root_hash(None, gv).unwrap().unwrap();
        assert_ne!(repaired_root, damaged_root);
        assert_clean(&missing, gv);

        // The record in storage changes nothing: the row is written again.
        let db = damaged(true);
        assert_eq!(db.root_hash(None, gv).unwrap().unwrap(), damaged_root);
        db.reconcile_indexed_tree_secondaries(psit, None, gv)
            .unwrap()
            .expect("reconcile");
        assert_eq!(db.root_hash(None, gv).unwrap().unwrap(), repaired_root);
        assert_clean(&db, gv);
        // The ranking proof follows the tree's links, and names the row.
        let ranking = crate::PathQuery::new_axis_top_k(
            vec![TEST_LEAF.to_vec(), b"psit".to_vec()],
            IndexAxis::Sum,
            20,
            0,
            true,
        );
        let proof = db.prove_query(&ranking, None, gv).unwrap().expect("prove");
        let crate::operations::proof::VerifiedPathQuery::AxisEntries {
            root_hash,
            entries: crate::operations::proof::indexed_axis::AxisEntries::Sum(proved),
            ..
        } = crate::GroveDb::verify_path_query(&proof, &ranking, gv).expect("verify")
        else {
            panic!("expected proved sum axis entries");
        };
        assert_eq!(root_hash, repaired_root);
        assert_eq!(proved.len(), rows.len());
        assert!(proved.iter().any(|entry| entry.primary_key == b"a"));

        // As released, the row is taken for present and left out of the
        // tree.
        let db = damaged(true);
        db.reconcile_indexed_tree_secondaries(psit, None, &as_released())
            .unwrap()
            .expect("reconcile as released");
        assert_eq!(db.root_hash(None, gv).unwrap().unwrap(), damaged_root);
    }

    /// A stray row no Merk links is removed from storage directly, and
    /// billed by the record the repair read, which a transaction that wrote
    /// the record itself holds and the committed state does not.
    #[cfg(grovedb_test_engine = "rocksdb")]
    #[test]
    fn a_stray_row_is_billed_by_what_the_transaction_holds() {
        use integer_encoding::VarInt;

        let gv = GroveVersion::latest();
        let psit: &[&[u8]] = &[TEST_LEAF, b"psit"];
        let stray_key = make_axis_secondary_key(IndexAxis::Sum, 0, 99, b"ghost");

        let removed_by_the_repair = |committed_first: bool| {
            let db = make_test_grovedb(gv);
            make_psit_with(&db, b"psit", REAL_ROWS, gv);
            let record = stored_rows(&db, psit, IndexAxis::Sum, gv).remove(0).1;
            let tx = db.start_transaction();
            store_unlinked_row(&db, psit, IndexAxis::Sum, &stray_key, &record, &tx, gv);
            let repair = if committed_first {
                db.commit_transaction(tx).unwrap().expect("commit");
                let repair = db.reconcile_indexed_tree_secondaries(psit, None, gv);
                assert_eq!(
                    stored_row_keys(&db, psit, IndexAxis::Sum, gv),
                    real_row_keys()
                );
                repair
            } else {
                db.reconcile_indexed_tree_secondaries(psit, Some(&tx), gv)
            };
            repair.value.expect("reconcile");
            (
                repair.cost.storage_cost.removed_bytes.total_removed_bytes(),
                record.len() as u32,
            )
        };

        let (committed, record_len) = removed_by_the_repair(true);
        let (staged, _) = removed_by_the_repair(false);
        assert_eq!(staged, committed);
        // The stored key is the 32-byte prefix of the secondary and the
        // row's key, and each length is billed with its own.
        let stored_key_len = 32 + stray_key.len() as u32;
        assert_eq!(
            committed,
            stored_key_len
                + stored_key_len.required_space() as u32
                + record_len
                + record_len.required_space() as u32
        );
    }

    /// A record in a secondary's storage that no tree links is no part of
    /// the tree a conversion starts from, and must be no part of what it
    /// builds: two databases with the same trees convert to the same root,
    /// whatever else their storage holds.
    #[cfg(grovedb_test_engine = "rocksdb")]
    #[test]
    fn a_record_no_tree_links_does_not_change_the_converted_root() {
        let gv = GroveVersion::latest();
        let psit: &[&[u8]] = &[TEST_LEAF, b"psit"];
        let rows: &[(&[u8], i64)] = &[(b"a", 1), (b"b", 1)];

        let clean = make_test_grovedb(gv);
        make_psit_with(&clean, b"psit", rows, gv);

        let with_record = make_test_grovedb(gv);
        make_psit_with(&with_record, b"psit", rows, gv);
        let record = stored_rows(&with_record, psit, IndexAxis::Sum, gv)
            .remove(0)
            .1;
        let tx = with_record.start_transaction();
        store_unlinked_row(
            &with_record,
            psit,
            IndexAxis::Sum,
            // It sorts before both rows, where it would be the first of the
            // rows the rebuild works through.
            &make_axis_secondary_key(IndexAxis::Sum, 0, -5, b"ghost"),
            &record,
            &tx,
            gv,
        );
        with_record.commit_transaction(tx).unwrap().expect("commit");
        assert_eq!(
            with_record.root_hash(None, gv).unwrap().unwrap(),
            clean.root_hash(None, gv).unwrap().unwrap(),
            "pre-condition: the record is in no tree"
        );

        for db in [&clean, &with_record] {
            db.convert_to_big_sum_tree([TEST_LEAF].as_ref(), b"psit", None, gv)
                .unwrap()
                .expect("convert");
            assert_clean(db, gv);
        }
        assert_eq!(
            with_record.root_hash(None, gv).unwrap().unwrap(),
            clean.root_hash(None, gv).unwrap().unwrap()
        );
        assert_eq!(
            stored_row_keys(&with_record, psit, IndexAxis::BigSum, gv),
            stored_row_keys(&clean, psit, IndexAxis::BigSum, gv)
        );
    }

    /// A version the repair does not know is refused before anything is
    /// opened, whatever the path.
    #[test]
    fn reconcile_refuses_a_version_it_does_not_know_before_reading() {
        let mut unknown = GroveVersion::latest().clone();
        unknown
            .grovedb_versions
            .operations
            .indexed_axis
            .reconcile_secondaries = 7;
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        make_psit_with(&db, b"psit", REAL_ROWS, gv);

        let indexed: &[&[u8]] = &[TEST_LEAF, b"psit"];
        let not_indexed: &[&[u8]] = &[TEST_LEAF];
        let root: &[&[u8]] = &[];
        for path in [indexed, not_indexed, root] {
            let repair = db.reconcile_indexed_tree_secondaries(path, None, &unknown);
            assert!(
                matches!(
                    repair.value,
                    Err(Error::VersionError(
                        grovedb_version::error::GroveVersionError::UnknownVersionMismatch { .. }
                    ))
                ),
                "{path:?}: {:?}",
                repair.value
            );
            assert_eq!(repair.cost, Default::default(), "{path:?}");
        }
    }

    /// The repair as GROVE_V4 runs it, on a tree written under GROVE_V4:
    /// the root, the rows left in storage and the cost are the ones measured
    /// on the commit this work started from (`2a79d035`), so the released
    /// repair is the one it was. A repair on the current version then
    /// finishes it.
    #[test]
    fn the_repair_on_grove_v4_is_what_it_was() {
        use grovedb_costs::{
            storage_cost::{removal::StorageRemovedBytes::BasicStorageRemoval, StorageCost},
            OperationCost,
        };
        use grovedb_version::version::v4::GROVE_V4;

        let v4 = &GROVE_V4;
        let psit: &[&[u8]] = &[TEST_LEAF, b"psit"];
        let db = make_test_grovedb(v4);
        make_psit_with(&db, b"psit", REAL_ROWS, v4);
        let strays: Vec<(Vec<u8>, Element)> = [-40i64, -20, 1, 2, 30, 50]
            .iter()
            .map(|sum| {
                (
                    make_axis_secondary_key(IndexAxis::Sum, 0, *sum, b"ghost"),
                    Element::new_sum_item(*sum),
                )
            })
            .collect();
        drift_axis_secondary(&db, psit, IndexAxis::Sum, &strays, &[], v4);

        let repair = db.reconcile_indexed_tree_secondaries(psit, None, v4);
        repair.value.expect("reconcile on GROVE_V4");
        assert_eq!(
            repair.cost,
            OperationCost {
                seek_count: 37,
                storage_cost: StorageCost {
                    added_bytes: 0,
                    replaced_bytes: 1263,
                    removed_bytes: BasicStorageRemoval(204),
                },
                storage_loaded_bytes: 4875,
                hash_node_calls: 38,
                sinsemilla_hash_calls: 0,
            }
        );
        assert_eq!(
            hex::encode(db.root_hash(None, v4).unwrap().unwrap()),
            "4bf77b9086ba63c400be89baf6ea657f4351ecdb8a8008b039c0ba2219971e7d"
        );
        // One of the six stray rows left storage. The other five are in no
        // tree and still there, between and around the two real rows.
        let left_in_storage: Vec<(String, usize)> = stored_rows(&db, psit, IndexAxis::Sum, v4)
            .into_iter()
            .map(|(key, record)| (hex::encode(key), record.len()))
            .collect();
        assert_eq!(
            left_in_storage,
            [
                ("7fffffffffffffec67686f7374", 72),
                ("7ffffffffffffffd62", 77),
                ("800000000000000167686f7374", 72),
                ("800000000000000267686f7374", 171),
                ("800000000000000761", 124),
                ("800000000000001e67686f7374", 123),
                ("800000000000003267686f7374", 72),
            ]
            .map(|(key, len)| (key.to_string(), len))
        );

        let gv = GroveVersion::latest();
        let root = db.root_hash(None, gv).unwrap().unwrap();
        db.reconcile_indexed_tree_secondaries(psit, None, gv)
            .unwrap()
            .expect("reconcile on the current version");
        assert_eq!(db.root_hash(None, gv).unwrap().unwrap(), root);
        assert_eq!(
            stored_row_keys(&db, psit, IndexAxis::Sum, gv),
            real_row_keys()
        );
        assert_clean(&db, gv);
    }
}
