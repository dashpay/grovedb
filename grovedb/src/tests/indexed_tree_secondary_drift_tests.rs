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

    use crate::{
        tests::{common::EMPTY_PATH, make_test_grovedb, TempGroveDb, TEST_LEAF},
        Element, Error,
    };

    // -----------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------

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
        let batch = StorageBatch::new();
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
            pristine_listing,
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
                .expect("top_k"),
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
                .expect("top_k"),
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
                .expect("top_k"),
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
                .expect("top_k"),
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
                .expect("top_k"),
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
                .expect("top_k"),
            vec![(1u64, b"small".to_vec())],
            "the drifted index must be missing 'big'"
        );

        db.reconcile_indexed_tree_secondaries([TEST_LEAF, b"pcit"].as_ref(), None, gv)
            .unwrap()
            .expect("reconcile rebuilds from the primary");

        assert_eq!(
            db.indexed_count_top_k([TEST_LEAF, b"pcit"].as_ref(), 10, true, None, gv)
                .unwrap()
                .expect("top_k"),
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
                .expect("top_k"),
            vec![(1u64, b"z".to_vec())],
            "only 'z' must be indexed"
        );
        assert_clean(&db, gv);
    }

    // -----------------------------------------------------------------
    // Malformed secondary rows must surface, not be silently dropped
    // -----------------------------------------------------------------

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
    /// carries no recoverable `(count, original_key)` pair. Every direct count
    /// query must raise `CorruptedData` naming the axis and the expected width
    /// — dropping the row instead would let a short-key row silently shrink a
    /// top-k page or a range listing while the caller sees a successful result.
    ///
    /// Each of the three query shapes decodes in a different place — the plain
    /// walk, the pagination *skip* loop, the pagination *collect* loop, and the
    /// range walk — so all four are driven here.
    #[test]
    fn a_short_secondary_key_is_reported_by_every_count_query_shape() {
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
        assert_short_key_corruption(
            db.indexed_count_top_k_paginated([TEST_LEAF, b"pcit"].as_ref(), 1, 1, true, None, gv)
                .unwrap()
                .expect_err("paginated skip must surface the malformed row"),
            "paginated (offset 1, skip loop)",
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

        // Ascending top-k stops before reaching the malformed row, so the
        // well-formed prefix of the index still reads cleanly — the error above
        // is the decoder refusing a specific row, not the query failing wholesale.
        assert_eq!(
            db.indexed_count_top_k([TEST_LEAF, b"pcit"].as_ref(), 2, false, None, gv)
                .unwrap()
                .expect("the first two rows are well formed"),
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
        let batch = StorageBatch::new();
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
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .expect("commit axis drift");
        tx.commit().expect("tx commit");
    }

    /// PSIT: an orphan row in the sum secondary is removed and the root
    /// returns to the pristine twin's — the repair covers the sum axis, not
    /// just count.
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
}
