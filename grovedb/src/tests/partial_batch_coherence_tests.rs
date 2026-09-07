//! `apply_partial_batch` coherence between its two segments (issue #842).
//!
//! The partial flow applies the initial segment, hands its pending cost
//! to the caller's add-on closure, then applies the closure's ops as a
//! continuation before one atomic commit. Both segments must observe one
//! coherent logical state: the continuation has to see every node write,
//! row delete, pending root key and root-metadata change the initial
//! segment made, and a failing continuation must leave nothing behind.
//!
//! Each case applies the same two op sets three ways — one combined
//! `apply_batch`, two sequential `apply_batch` calls, and one
//! `apply_partial_batch` — and pins that the partial flow lands on the
//! sequential flow's exact state (root hash included: each segment is one
//! Merk apply on the same tree, exactly like two sequential batches), that
//! every flow reads the same rows, and that `verify_grovedb` stays clean.

#[cfg(test)]
mod tests {
    use grovedb_merk::tree_type::TreeType;
    use grovedb_version::version::GroveVersion;

    use crate::{
        batch::{BatchApplyOptions, QualifiedGroveDbOp, SubelementsDeletionBehavior},
        tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
        Element, Error, GroveDb, IndexedAxisEntrySliceExt,
    };

    fn assert_verify_clean(db: &GroveDb, grove_version: &GroveVersion) {
        let issues = db
            .verify_grovedb(None, true, true, grove_version)
            .expect("verify_grovedb must not hard-error");
        assert!(issues.is_empty(), "verify_grovedb reported: {issues:?}");
    }

    fn root_hash(db: &GroveDb, grove_version: &GroveVersion) -> [u8; 32] {
        db.root_hash(None, grove_version)
            .unwrap()
            .expect("root hash")
    }

    /// A PCIT at `[TEST_LEAF, "cidx"]` with count-tree groups, each holding
    /// `count` items keyed `0..count`.
    fn insert_pcit_with_groups(
        db: &GroveDb,
        cidx: &[u8],
        groups: &[(&[u8], u64)],
        grove_version: &GroveVersion,
    ) {
        db.insert(
            [TEST_LEAF].as_ref(),
            cidx,
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert PCIT");
        for (group, count) in groups {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, cidx].as_ref(),
                group,
                Element::empty_count_tree(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert group");
            for i in 0..*count {
                db.insert(
                    [TEST_LEAF, cidx, group].as_ref(),
                    &i.to_be_bytes(),
                    Element::new_item(vec![]),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("populate group");
            }
        }
    }

    fn two_group_pcit(grove_version: &GroveVersion) -> TempGroveDb {
        let db = make_test_grovedb(grove_version);
        insert_pcit_with_groups(&db, b"cidx", &[(b"p", 2), (b"q", 2)], grove_version);
        db
    }

    fn item_op(path: Vec<Vec<u8>>, key: u64) -> QualifiedGroveDbOp {
        QualifiedGroveDbOp::insert_or_replace_op(
            path,
            key.to_be_bytes().to_vec(),
            Element::new_item(vec![]),
        )
    }

    /// Insert one more item into a PCIT group: its count bumps and the
    /// mirror row moves from `(old ‖ group)` to `(new ‖ group)`.
    fn bump(cidx: &[u8], group: &[u8], key: u64) -> QualifiedGroveDbOp {
        item_op(vec![TEST_LEAF.to_vec(), cidx.to_vec(), group.to_vec()], key)
    }

    fn top_k(db: &GroveDb, cidx: &[u8], grove_version: &GroveVersion) -> Vec<(u64, Vec<u8>)> {
        db.indexed_count_top_k([TEST_LEAF, cidx].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("top-k")
            .key_pairs()
    }

    fn apply_partial(
        db: &GroveDb,
        first: Vec<QualifiedGroveDbOp>,
        second: Vec<QualifiedGroveDbOp>,
        options: Option<BatchApplyOptions>,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        db.apply_partial_batch(
            first,
            options,
            move |_cost, _left_over| Ok(second.clone()),
            None,
            grove_version,
        )
        .unwrap()
    }

    /// Apply `first` then `second` three ways on identically prepared
    /// databases and pin the equivalence described in the module docs.
    /// `read` snapshots whatever rows the case cares about.
    fn assert_three_flows_agree<R: PartialEq + std::fmt::Debug>(
        setup: impl Fn(&GroveVersion) -> TempGroveDb,
        first: Vec<QualifiedGroveDbOp>,
        second: Vec<QualifiedGroveDbOp>,
        options: Option<BatchApplyOptions>,
        read: impl Fn(&GroveDb, &GroveVersion) -> R,
        grove_version: &GroveVersion,
    ) -> R {
        let combined = setup(grove_version);
        combined
            .apply_batch(
                first.iter().chain(second.iter()).cloned().collect(),
                options.clone(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("combined batch applies");

        let sequential = setup(grove_version);
        sequential
            .apply_batch(first.clone(), options.clone(), None, grove_version)
            .unwrap()
            .expect("first batch applies");
        sequential
            .apply_batch(second.clone(), options.clone(), None, grove_version)
            .unwrap()
            .expect("second batch applies");

        let partial = setup(grove_version);
        apply_partial(&partial, first, second, options, grove_version)
            .expect("partial batch applies");

        assert_verify_clean(&combined, grove_version);
        assert_verify_clean(&sequential, grove_version);
        assert_verify_clean(&partial, grove_version);

        let rows = read(&combined, grove_version);
        assert_eq!(
            read(&sequential, grove_version),
            rows,
            "sequential flow reads differ from the combined batch"
        );
        assert_eq!(
            read(&partial, grove_version),
            rows,
            "partial flow reads differ from the combined batch"
        );
        assert_eq!(
            root_hash(&partial, grove_version),
            root_hash(&sequential, grove_version),
            "partial flow must land on the sequential flow's root hash"
        );
        rows
    }

    // -----------------------------------------------------------------
    // Overlapping secondaries (the issue's reproduction)
    // -----------------------------------------------------------------

    #[test]
    fn same_secondary_rekeyed_in_both_segments() {
        // Issue #842: the initial segment re-keys `p` (2 → 3), the
        // continuation re-keys `q` (2 → 3) in the SAME secondary. The old
        // `(2 ‖ p)` row must not survive.
        let grove_version = GroveVersion::latest();
        let rows = assert_three_flows_agree(
            two_group_pcit,
            vec![bump(b"cidx", b"p", 2)],
            vec![bump(b"cidx", b"q", 2)],
            Some(BatchApplyOptions::default()),
            |db, gv| top_k(db, b"cidx", gv),
            grove_version,
        );
        assert_eq!(rows, vec![(3, b"q".to_vec()), (3, b"p".to_vec())]);
    }

    #[test]
    fn same_group_rekeyed_in_both_segments() {
        // Both segments bump the SAME group: the continuation must start
        // from the initial segment's count (3), not the committed one (2),
        // or the mirror lands on `(3 ‖ p)` with a stale pre-state.
        let grove_version = GroveVersion::latest();
        let rows = assert_three_flows_agree(
            two_group_pcit,
            vec![bump(b"cidx", b"p", 2)],
            vec![bump(b"cidx", b"p", 3)],
            Some(BatchApplyOptions::default()),
            |db, gv| top_k(db, b"cidx", gv),
            grove_version,
        );
        assert_eq!(rows, vec![(4, b"p".to_vec()), (2, b"q".to_vec())]);
    }

    #[test]
    fn disjoint_secondaries_in_both_segments() {
        // Two PCITs: each segment re-keys a row in its own secondary.
        let grove_version = GroveVersion::latest();
        let setup = |gv: &GroveVersion| {
            let db = make_test_grovedb(gv);
            insert_pcit_with_groups(&db, b"cidx1", &[(b"p", 2), (b"q", 2)], gv);
            insert_pcit_with_groups(&db, b"cidx2", &[(b"p", 2), (b"q", 2)], gv);
            db
        };
        let rows = assert_three_flows_agree(
            setup,
            vec![bump(b"cidx1", b"p", 2)],
            vec![bump(b"cidx2", b"q", 2)],
            Some(BatchApplyOptions::default()),
            |db, gv| (top_k(db, b"cidx1", gv), top_k(db, b"cidx2", gv)),
            grove_version,
        );
        assert_eq!(
            rows,
            (
                vec![(3, b"p".to_vec()), (2, b"q".to_vec())],
                vec![(3, b"q".to_vec()), (2, b"p".to_vec())],
            )
        );
    }

    // -----------------------------------------------------------------
    // Deletes in the initial segment
    // -----------------------------------------------------------------

    #[test]
    fn primary_row_delete_then_continuation_insert_into_same_group() {
        // Initial segment deletes an item from `p` (2 → 1, mirror row
        // moves down); the continuation inserts a new item into `p`
        // (1 → 2, mirror row moves back). The continuation must see the
        // delete: group `p` ends with exactly {1, 7}.
        let grove_version = GroveVersion::latest();
        let rows = assert_three_flows_agree(
            two_group_pcit,
            vec![QualifiedGroveDbOp::delete_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec(), b"p".to_vec()],
                0u64.to_be_bytes().to_vec(),
            )],
            vec![bump(b"cidx", b"p", 7)],
            Some(BatchApplyOptions::default()),
            |db, gv| {
                let present: Vec<u64> = [0u64, 1, 7]
                    .into_iter()
                    .filter(|k| {
                        db.get_raw(
                            [TEST_LEAF, b"cidx", b"p"].as_ref().into(),
                            &k.to_be_bytes(),
                            None,
                            gv,
                        )
                        .unwrap()
                        .is_ok()
                    })
                    .collect();
                (top_k(db, b"cidx", gv), present)
            },
            grove_version,
        );
        assert_eq!(
            rows,
            (vec![(2, b"q".to_vec()), (2, b"p".to_vec())], vec![1, 7])
        );
    }

    #[test]
    fn group_delete_then_continuation_inserts_sibling_group() {
        // Initial segment drops the whole `p` group (its mirror row goes
        // away); the continuation bumps `q` in the same secondary. The
        // drained row must not come back.
        let grove_version = GroveVersion::latest();
        let rows = assert_three_flows_agree(
            two_group_pcit,
            vec![QualifiedGroveDbOp::delete_tree_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                b"p".to_vec(),
                TreeType::CountTree,
                SubelementsDeletionBehavior::DeleteChildren,
            )],
            vec![bump(b"cidx", b"q", 2)],
            Some(BatchApplyOptions::default()),
            |db, gv| top_k(db, b"cidx", gv),
            grove_version,
        );
        assert_eq!(rows, vec![(3, b"q".to_vec())]);
    }

    // -----------------------------------------------------------------
    // Ordinary (non-indexed) subtrees shared between segments
    // -----------------------------------------------------------------

    fn ordinary_subtree(grove_version: &GroveVersion) -> TempGroveDb {
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"sub",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert subtree");
        for i in 0..8u64 {
            db.insert(
                [TEST_LEAF, b"sub"].as_ref(),
                &i.to_be_bytes(),
                Element::new_item(vec![i as u8]),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("populate subtree");
        }
        db
    }

    fn present_keys(db: &GroveDb, path: &[&[u8]], grove_version: &GroveVersion) -> Vec<u64> {
        (0..40u64)
            .filter(|k| {
                db.get_raw(path.into(), &k.to_be_bytes(), None, grove_version)
                    .unwrap()
                    .is_ok()
            })
            .collect()
    }

    #[test]
    fn same_ordinary_subtree_in_both_segments() {
        // Both segments write into `[TEST_LEAF, "sub"]`, a subtree below
        // the pause height whose parent element the initial segment
        // already rewrote. Every key from both segments must survive.
        let grove_version = GroveVersion::latest();
        let sub = vec![TEST_LEAF.to_vec(), b"sub".to_vec()];
        let rows = assert_three_flows_agree(
            ordinary_subtree,
            (10..20u64).map(|k| item_op(sub.clone(), k)).collect(),
            (20..30u64).map(|k| item_op(sub.clone(), k)).collect(),
            Some(BatchApplyOptions::default()),
            |db, gv| present_keys(db, &[TEST_LEAF, b"sub"], gv),
            grove_version,
        );
        assert_eq!(
            rows,
            (0..8u64).chain(10..30).collect::<Vec<_>>(),
            "the setup's 0..8 plus both segments' 10..30"
        );
    }

    #[test]
    fn pause_level_subtree_shared_between_segments() {
        // Both segments write into `[TEST_LEAF]` itself — the Merk AT the
        // pause height, whose new root key lives only in the initial
        // segment's leftover root-level op. The continuation must reuse
        // that live Merk rather than reopen it from the committed root
        // key.
        let grove_version = GroveVersion::latest();
        let leaf = vec![TEST_LEAF.to_vec()];
        let rows = assert_three_flows_agree(
            make_test_grovedb,
            (10..20u64).map(|k| item_op(leaf.clone(), k)).collect(),
            (20..30u64).map(|k| item_op(leaf.clone(), k)).collect(),
            Some(BatchApplyOptions::default()),
            |db, gv| present_keys(db, &[TEST_LEAF], gv),
            grove_version,
        );
        assert_eq!(rows, (10..30u64).collect::<Vec<_>>());
    }

    #[test]
    fn new_tree_from_initial_segment_populated_by_continuation() {
        // The initial segment creates an empty subtree at the pause
        // height (a root-level leftover op); the continuation writes into
        // it. The continuation must find the pending empty Merk.
        let grove_version = GroveVersion::latest();
        let rows = assert_three_flows_agree(
            make_test_grovedb,
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![],
                b"fresh".to_vec(),
                Element::empty_tree(),
            )],
            (0..5u64)
                .map(|k| item_op(vec![b"fresh".to_vec()], k))
                .collect(),
            Some(BatchApplyOptions::default()),
            |db, gv| present_keys(db, &[b"fresh"], gv),
            grove_version,
        );
        assert_eq!(rows, (0..5u64).collect::<Vec<_>>());
    }

    fn count_child_op(cidx_path: Vec<Vec<u8>>, group: &[u8]) -> QualifiedGroveDbOp {
        QualifiedGroveDbOp::insert_or_replace_op(
            cidx_path,
            group.to_vec(),
            Element::empty_provable_count_tree(),
        )
    }

    #[test]
    fn new_pcit_from_initial_segment_populated_by_continuation() {
        // The initial segment creates an EMPTY indexed tree under
        // TEST_LEAF (its element is written into the cached TEST_LEAF
        // Merk, nowhere in storage); the continuation adds a group and
        // fills it. The continuation must find the primary AND open its
        // secondaries from the in-batch element, then mirror the derived
        // count.
        let grove_version = GroveVersion::latest();
        let cidx_path = vec![TEST_LEAF.to_vec(), b"cidx".to_vec()];
        let mut second = vec![count_child_op(cidx_path.clone(), b"a")];
        second.extend((0..3u64).map(|k| bump(b"cidx", b"a", k)));
        let rows = assert_three_flows_agree(
            make_test_grovedb,
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"cidx".to_vec(),
                Element::empty_provable_count_indexed_tree(),
            )],
            second,
            Some(BatchApplyOptions::default()),
            |db, gv| top_k(db, b"cidx", gv),
            grove_version,
        );
        assert_eq!(rows, vec![(3, b"a".to_vec())]);
    }

    #[test]
    fn new_root_level_pcit_from_initial_segment_populated_by_continuation() {
        // Same shape, but the indexed tree sits at the ROOT: its creating
        // op is a root-level leftover the initial segment never executed,
        // so the element exists only in that pending op.
        let grove_version = GroveVersion::latest();
        let cidx_path = vec![b"cidx".to_vec()];
        let mut second = vec![count_child_op(cidx_path.clone(), b"a")];
        second.extend((0..3u64).map(|k| item_op(vec![b"cidx".to_vec(), b"a".to_vec()], k)));
        let rows = assert_three_flows_agree(
            make_test_grovedb,
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![],
                b"cidx".to_vec(),
                Element::empty_provable_count_indexed_tree(),
            )],
            second,
            Some(BatchApplyOptions::default()),
            |db, gv| {
                db.indexed_count_top_k([b"cidx"].as_ref(), 10, true, None, gv)
                    .unwrap()
                    .expect("top-k")
                    .key_pairs()
            },
            grove_version,
        );
        assert_eq!(rows, vec![(3, b"a".to_vec())]);
    }

    // -----------------------------------------------------------------
    // Cross-segment gate: shapes no cache can make coherent are refused
    // -----------------------------------------------------------------

    #[test]
    fn continuation_write_under_path_deleted_by_initial_segment_is_refused() {
        let grove_version = GroveVersion::latest();
        let db = two_group_pcit(grove_version);
        let before = root_hash(&db, grove_version);

        let result = apply_partial(
            &db,
            vec![QualifiedGroveDbOp::delete_tree_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                b"p".to_vec(),
                TreeType::CountTree,
                SubelementsDeletionBehavior::DeleteChildren,
            )],
            vec![bump(b"cidx", b"p", 7)],
            Some(BatchApplyOptions::default()),
            grove_version,
        );
        assert!(
            matches!(result, Err(Error::InvalidBatchOperation(_))),
            "{result:?}"
        );
        assert_untouched(&db, before, grove_version);
    }

    #[test]
    fn continuation_delete_of_subtree_initial_segment_wrote_into_is_refused() {
        let grove_version = GroveVersion::latest();
        let db = two_group_pcit(grove_version);
        let before = root_hash(&db, grove_version);

        let result = apply_partial(
            &db,
            vec![bump(b"cidx", b"p", 2)],
            vec![QualifiedGroveDbOp::delete_tree_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                b"p".to_vec(),
                TreeType::CountTree,
                SubelementsDeletionBehavior::DeleteChildren,
            )],
            Some(BatchApplyOptions::default()),
            grove_version,
        );
        assert!(
            matches!(result, Err(Error::InvalidBatchOperation(_))),
            "{result:?}"
        );
        assert_untouched(&db, before, grove_version);
    }

    #[test]
    fn continuation_replace_of_subtree_initial_segment_wrote_into_is_refused() {
        let grove_version = GroveVersion::latest();
        let db = two_group_pcit(grove_version);
        let before = root_hash(&db, grove_version);

        let result = apply_partial(
            &db,
            vec![bump(b"cidx", b"p", 2)],
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"cidx".to_vec(),
                Element::empty_provable_count_indexed_tree(),
            )],
            Some(BatchApplyOptions::default()),
            grove_version,
        );
        assert!(
            matches!(result, Err(Error::InvalidBatchOperation(_))),
            "{result:?}"
        );
        assert_untouched(&db, before, grove_version);
    }

    #[test]
    fn continuation_overwrite_of_committed_pcit_with_empty_one_cleans_up() {
        // Add-on ops get the indexed-overwrite preflight too. Overwriting
        // a committed, populated indexed tree with an EMPTY one is the
        // safe-subset overwrite it allows (the old primary subtrees and
        // secondaries are swept at commit), so the partial flow must land
        // on the same clean, empty tree as the other flows.
        let grove_version = GroveVersion::latest();
        let rows = assert_three_flows_agree(
            two_group_pcit,
            vec![item_op(vec![TEST_LEAF.to_vec()], 1)],
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"cidx".to_vec(),
                Element::empty_provable_count_indexed_tree(),
            )],
            Some(BatchApplyOptions::default()),
            |db, gv| top_k(db, b"cidx", gv),
            grove_version,
        );
        assert!(rows.is_empty(), "{rows:?}");
    }

    // -----------------------------------------------------------------
    // Root metadata (pause height 0: the initial segment writes the base
    // root key itself)
    // -----------------------------------------------------------------

    #[test]
    fn root_metadata_written_by_both_segments() {
        let grove_version = GroveVersion::latest();
        let options = Some(BatchApplyOptions {
            batch_pause_height: Some(0),
            ..Default::default()
        });
        let rows = assert_three_flows_agree(
            make_test_grovedb,
            (10..20u64).map(|k| item_op(vec![], k)).collect(),
            (20..30u64).map(|k| item_op(vec![], k)).collect(),
            options,
            |db, gv| present_keys(db, &[], gv),
            grove_version,
        );
        assert_eq!(rows, (10..30u64).collect::<Vec<_>>());
    }

    // -----------------------------------------------------------------
    // Rollback: a failing continuation commits nothing
    // -----------------------------------------------------------------

    fn assert_untouched(db: &GroveDb, before: [u8; 32], grove_version: &GroveVersion) {
        assert_eq!(root_hash(db, grove_version), before, "root hash changed");
        assert_eq!(
            top_k(db, b"cidx", grove_version),
            vec![(2, b"q".to_vec()), (2, b"p".to_vec())],
            "rows changed"
        );
        assert_verify_clean(db, grove_version);
    }

    #[test]
    fn continuation_closure_error_commits_nothing() {
        let grove_version = GroveVersion::latest();
        let db = two_group_pcit(grove_version);
        let before = root_hash(&db, grove_version);

        let result = db
            .apply_partial_batch(
                vec![bump(b"cidx", b"p", 2)],
                Some(BatchApplyOptions::default()),
                |_cost, _left_over| Err(Error::InvalidInput("caller aborted")),
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInput(_))), "{result:?}");
        assert_untouched(&db, before, grove_version);
    }

    #[test]
    fn continuation_apply_error_commits_nothing() {
        // The add-on ops themselves fail mid-apply (a write under a path
        // that does not exist): the initial segment must roll back too.
        let grove_version = GroveVersion::latest();
        let db = two_group_pcit(grove_version);
        let before = root_hash(&db, grove_version);

        let result = apply_partial(
            &db,
            vec![bump(b"cidx", b"p", 2)],
            vec![item_op(vec![TEST_LEAF.to_vec(), b"missing".to_vec()], 1)],
            Some(BatchApplyOptions::default()),
            grove_version,
        );
        assert!(result.is_err(), "continuation must fail");
        assert_untouched(&db, before, grove_version);
    }

    #[test]
    fn continuation_error_inside_caller_transaction_leaves_it_clean() {
        // With a caller-owned transaction the failed partial batch must
        // not leave its initial segment staged in the transaction: the
        // caller committing afterwards must commit nothing from it.
        let grove_version = GroveVersion::latest();
        let db = two_group_pcit(grove_version);
        let before = root_hash(&db, grove_version);

        let tx = db.start_transaction();
        let result = db
            .apply_partial_batch(
                vec![bump(b"cidx", b"p", 2)],
                Some(BatchApplyOptions::default()),
                |_cost, _left_over| Err(Error::InvalidInput("caller aborted")),
                Some(&tx),
                grove_version,
            )
            .unwrap();
        assert!(result.is_err());
        assert_eq!(
            db.root_hash(Some(&tx), grove_version).unwrap().unwrap(),
            before,
            "the transaction must not carry the initial segment"
        );
        db.commit_transaction(tx).unwrap().expect("commit");
        assert_untouched(&db, before, grove_version);
    }

    #[test]
    fn success_inside_caller_transaction_is_visible_only_after_commit() {
        let grove_version = GroveVersion::latest();
        let db = two_group_pcit(grove_version);
        let before = root_hash(&db, grove_version);

        let tx = db.start_transaction();
        db.apply_partial_batch(
            vec![bump(b"cidx", b"p", 2)],
            Some(BatchApplyOptions::default()),
            |_cost, _left_over| Ok(vec![bump(b"cidx", b"q", 2)]),
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("partial batch applies");
        assert_eq!(
            root_hash(&db, grove_version),
            before,
            "nothing committed yet"
        );
        assert_eq!(
            db.indexed_count_top_k(
                [TEST_LEAF, b"cidx"].as_ref(),
                10,
                true,
                Some(&tx),
                grove_version
            )
            .unwrap()
            .expect("top-k in tx")
            .key_pairs(),
            vec![(3, b"q".to_vec()), (3, b"p".to_vec())]
        );
        db.commit_transaction(tx).unwrap().expect("commit");
        assert_eq!(
            top_k(&db, b"cidx", grove_version),
            vec![(3, b"q".to_vec()), (3, b"p".to_vec())]
        );
        assert_verify_clean(&db, grove_version);
    }
}
