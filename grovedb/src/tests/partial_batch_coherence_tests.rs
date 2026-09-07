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
        reference_path::ReferencePathType,
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
    fn new_pcit_via_insert_if_not_exists_populated_by_continuation() {
        // Same shape as above, but the indexed tree is created with
        // InsertIfNotExists: the continuation's secondary opener must find
        // the pending element through that op variant too.
        let grove_version = GroveVersion::latest();
        let cidx_path = vec![TEST_LEAF.to_vec(), b"cidx".to_vec()];
        let mut second = vec![count_child_op(cidx_path.clone(), b"a")];
        second.extend((0..3u64).map(|k| bump(b"cidx", b"a", k)));
        let rows = assert_three_flows_agree(
            make_test_grovedb,
            vec![QualifiedGroveDbOp::insert_if_not_exists_op(
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
    fn continuation_write_under_pcit_overwritten_by_initial_segment_is_refused() {
        // The initial segment safe-subset-OVERWRITES a committed indexed
        // tree; the continuation writes under it. The old element's storage
        // is swept at commit, which would clear the continuation's writes —
        // the same shape one combined batch refuses with NotSupported — so
        // the partial flow must refuse it too, committing nothing.
        let grove_version = GroveVersion::latest();
        let db = two_group_pcit(grove_version);
        let before = root_hash(&db, grove_version);

        let overwrite = QualifiedGroveDbOp::replace_op(
            vec![TEST_LEAF.to_vec()],
            b"cidx".to_vec(),
            Element::empty_provable_count_indexed_tree(),
        );
        let populate = count_child_op(vec![TEST_LEAF.to_vec(), b"cidx".to_vec()], b"a");

        let combined = db.apply_batch(
            vec![overwrite.clone(), populate.clone()],
            Some(BatchApplyOptions::default()),
            None,
            grove_version,
        );
        assert!(
            matches!(combined.unwrap(), Err(Error::NotSupported(_))),
            "the combined batch must refuse overwrite-then-populate"
        );

        let result = apply_partial(
            &db,
            vec![overwrite],
            vec![populate],
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
    fn continuation_replace_overwrite_of_committed_pcit_with_empty_one_cleans_up() {
        // The Replace-op variant of the safe-subset overwrite from the
        // continuation (no writes under it afterwards): allowed, and the
        // old rows are swept exactly like the InsertOrReplace variant.
        let grove_version = GroveVersion::latest();
        let rows = assert_three_flows_agree(
            two_group_pcit,
            vec![item_op(vec![TEST_LEAF.to_vec()], 1)],
            vec![QualifiedGroveDbOp::replace_op(
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

    // -----------------------------------------------------------------
    // Cross-segment gate: overwrites are refused whatever the new element
    // is — a plain item over a written-into subtree orphans its pending
    // child writes just like a fresh tree would
    // -----------------------------------------------------------------

    #[test]
    fn continuation_nontree_overwrite_of_subtree_initial_segment_wrote_into_is_refused() {
        let grove_version = GroveVersion::latest();
        let db = two_group_pcit(grove_version);
        let before = root_hash(&db, grove_version);

        let result = apply_partial(
            &db,
            vec![bump(b"cidx", b"p", 2)],
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"cidx".to_vec(),
                Element::new_item(vec![]),
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

    // -----------------------------------------------------------------
    // Ordinary apply_batch has no continuation, so it must not pause
    // (issue #889: it used to commit the paused result and silently
    // discard the leftover ancestor propagation ops)
    // -----------------------------------------------------------------

    #[test]
    fn ordinary_batch_with_nonzero_pause_height_is_refused() {
        let grove_version = GroveVersion::latest();
        let db = two_group_pcit(grove_version);
        let before = root_hash(&db, grove_version);

        let result = db
            .apply_batch(
                vec![bump(b"cidx", b"p", 2)],
                Some(BatchApplyOptions {
                    batch_pause_height: Some(1),
                    ..Default::default()
                }),
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            matches!(result, Err(Error::InvalidBatchOperation(_))),
            "{result:?}"
        );
        assert_untouched(&db, before, grove_version);
    }

    // -----------------------------------------------------------------
    // Add-on DeleteTree ops get the SubelementsDeletionBehavior preflight
    // (issue #709): the Error emptiness check, Skip filtering, and the
    // descendant storage cleanup all apply to the continuation too
    // -----------------------------------------------------------------

    #[test]
    fn continuation_delete_tree_error_behavior_on_nonempty_tree_is_refused() {
        let grove_version = GroveVersion::latest();
        let db = ordinary_subtree(grove_version);
        let before = root_hash(&db, grove_version);

        let result = apply_partial(
            &db,
            vec![item_op(vec![TEST_LEAF.to_vec()], 10)],
            vec![QualifiedGroveDbOp::delete_tree_op(
                vec![TEST_LEAF.to_vec()],
                b"sub".to_vec(),
                TreeType::NormalTree,
                SubelementsDeletionBehavior::Error,
            )],
            Some(BatchApplyOptions::default()),
            grove_version,
        );
        assert!(
            matches!(result, Err(Error::DeletingNonEmptyTree(_))),
            "{result:?}"
        );
        assert_eq!(root_hash(&db, grove_version), before, "root hash changed");
        assert_eq!(
            present_keys(&db, &[TEST_LEAF, b"sub"], grove_version),
            (0..8u64).collect::<Vec<_>>(),
            "the non-empty tree must survive intact"
        );
        assert_verify_clean(&db, grove_version);
    }

    #[test]
    fn continuation_delete_tree_skip_behavior_on_nonempty_tree_skips() {
        // A Skip DeleteTree in the continuation of a non-empty tree is
        // filtered out, exactly like in the initial segment: the tree
        // survives and the rest of the continuation still applies.
        let grove_version = GroveVersion::latest();
        let rows = assert_three_flows_agree(
            ordinary_subtree,
            vec![item_op(vec![TEST_LEAF.to_vec()], 10)],
            vec![
                QualifiedGroveDbOp::delete_tree_op(
                    vec![TEST_LEAF.to_vec()],
                    b"sub".to_vec(),
                    TreeType::NormalTree,
                    SubelementsDeletionBehavior::Skip,
                ),
                item_op(vec![TEST_LEAF.to_vec()], 11),
            ],
            Some(BatchApplyOptions::default()),
            |db, gv| {
                (
                    present_keys(db, &[TEST_LEAF, b"sub"], gv),
                    present_keys(db, &[TEST_LEAF], gv),
                )
            },
            grove_version,
        );
        assert_eq!(rows, ((0..8u64).collect::<Vec<_>>(), vec![10, 11]));
    }

    #[test]
    fn continuation_delete_tree_delete_children_cleans_descendants() {
        let grove_version = GroveVersion::latest();
        let rows = assert_three_flows_agree(
            ordinary_subtree,
            vec![item_op(vec![TEST_LEAF.to_vec()], 10)],
            vec![QualifiedGroveDbOp::delete_tree_op(
                vec![TEST_LEAF.to_vec()],
                b"sub".to_vec(),
                TreeType::NormalTree,
                SubelementsDeletionBehavior::DeleteChildren,
            )],
            Some(BatchApplyOptions::default()),
            |db, gv| {
                (
                    db.get_raw([TEST_LEAF].as_ref().into(), b"sub", None, gv)
                        .unwrap()
                        .is_ok(),
                    present_keys(db, &[TEST_LEAF], gv),
                )
            },
            grove_version,
        );
        assert_eq!(rows, (false, vec![10]));
    }

    #[test]
    fn continuation_delete_tree_delete_children_reinsert_produces_clean_tree() {
        // Pin the descendant STORAGE cleanup the behavior map drives: after
        // the continuation's DeleteChildren, re-creating the same tree must
        // produce a genuinely empty one with none of the old rows.
        let grove_version = GroveVersion::latest();
        let db = ordinary_subtree(grove_version);

        apply_partial(
            &db,
            vec![item_op(vec![TEST_LEAF.to_vec()], 10)],
            vec![QualifiedGroveDbOp::delete_tree_op(
                vec![TEST_LEAF.to_vec()],
                b"sub".to_vec(),
                TreeType::NormalTree,
                SubelementsDeletionBehavior::DeleteChildren,
            )],
            Some(BatchApplyOptions::default()),
            grove_version,
        )
        .expect("partial batch applies");

        db.insert(
            [TEST_LEAF].as_ref(),
            b"sub",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("re-insert tree at the same path");
        assert!(
            present_keys(&db, &[TEST_LEAF, b"sub"], grove_version).is_empty(),
            "old rows must not survive into the re-created tree"
        );
        assert_verify_clean(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // Add-on typed appends are preprocessed like the initial segment's
    // (issue #895: they used to be silently dropped), and appends whose
    // target the initial segment wrote are refused — their preprocessing
    // reads committed state no cache can make coherent
    // -----------------------------------------------------------------

    fn ordinary_subtree_with_mmr(grove_version: &GroveVersion) -> TempGroveDb {
        let db = ordinary_subtree(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"mmr",
            Element::empty_mmr_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert mmr tree");
        db
    }

    #[test]
    fn continuation_typed_append_is_executed() {
        let grove_version = GroveVersion::latest();
        let rows = assert_three_flows_agree(
            ordinary_subtree_with_mmr,
            vec![item_op(vec![TEST_LEAF.to_vec()], 10)],
            vec![QualifiedGroveDbOp::mmr_tree_append_op(
                vec![TEST_LEAF.to_vec(), b"mmr".to_vec()],
                b"leaf-0".to_vec(),
            )],
            Some(BatchApplyOptions::default()),
            |db, gv| {
                (
                    db.mmr_tree_leaf_count([TEST_LEAF].as_ref(), b"mmr", None, gv)
                        .unwrap()
                        .expect("leaf count"),
                    db.mmr_tree_get_value([TEST_LEAF].as_ref(), b"mmr", 0, None, gv)
                        .unwrap()
                        .expect("leaf value"),
                )
            },
            grove_version,
        );
        assert_eq!(rows, (1, Some(b"leaf-0".to_vec())));
    }

    #[test]
    fn continuation_typed_append_to_tree_initial_segment_appended_is_refused() {
        let grove_version = GroveVersion::latest();
        let db = ordinary_subtree_with_mmr(grove_version);
        let before = root_hash(&db, grove_version);

        let result = apply_partial(
            &db,
            vec![QualifiedGroveDbOp::mmr_tree_append_op(
                vec![TEST_LEAF.to_vec(), b"mmr".to_vec()],
                b"a".to_vec(),
            )],
            vec![QualifiedGroveDbOp::mmr_tree_append_op(
                vec![TEST_LEAF.to_vec(), b"mmr".to_vec()],
                b"b".to_vec(),
            )],
            Some(BatchApplyOptions::default()),
            grove_version,
        );
        assert!(
            matches!(result, Err(Error::InvalidBatchOperation(_))),
            "{result:?}"
        );
        assert_eq!(root_hash(&db, grove_version), before, "root hash changed");
        assert_eq!(
            db.mmr_tree_leaf_count([TEST_LEAF].as_ref(), b"mmr", None, grove_version)
                .unwrap()
                .expect("leaf count"),
            0,
            "neither append may land"
        );
        assert_verify_clean(&db, grove_version);
    }

    #[test]
    fn continuation_delete_of_non_merk_tree_initial_segment_appended_to_is_refused() {
        let grove_version = GroveVersion::latest();
        let db = ordinary_subtree_with_mmr(grove_version);
        let before = root_hash(&db, grove_version);

        let result = apply_partial(
            &db,
            vec![QualifiedGroveDbOp::mmr_tree_append_op(
                vec![TEST_LEAF.to_vec(), b"mmr".to_vec()],
                b"a".to_vec(),
            )],
            vec![QualifiedGroveDbOp::delete_op(
                vec![TEST_LEAF.to_vec()],
                b"mmr".to_vec(),
            )],
            Some(BatchApplyOptions::default()),
            grove_version,
        );
        assert!(
            matches!(result, Err(Error::InvalidBatchOperation(_))),
            "{result:?}"
        );
        assert_eq!(root_hash(&db, grove_version), before, "root hash changed");
        assert_verify_clean(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // Add-on reference ops resolve targets the initial segment wrote the
    // way one combined batch would: through the segment's in-batch op
    // -----------------------------------------------------------------

    fn reference_op_to(path: Vec<Vec<u8>>, key: &[u8], target: Vec<Vec<u8>>) -> QualifiedGroveDbOp {
        QualifiedGroveDbOp::insert_or_replace_op(
            path,
            key.to_vec(),
            Element::new_reference(ReferencePathType::AbsolutePathReference(target)),
        )
    }

    #[test]
    fn continuation_reference_to_item_rewritten_by_initial_segment() {
        // The initial segment rewrites `sub/0`; the continuation inserts a
        // reference to it. The reference must commit to the segment's NEW
        // value, exactly as a combined batch would.
        let grove_version = GroveVersion::latest();
        let sub = vec![TEST_LEAF.to_vec(), b"sub".to_vec()];
        let target = vec![
            TEST_LEAF.to_vec(),
            b"sub".to_vec(),
            0u64.to_be_bytes().to_vec(),
        ];
        let rows = assert_three_flows_agree(
            ordinary_subtree,
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                sub,
                0u64.to_be_bytes().to_vec(),
                Element::new_item(vec![9]),
            )],
            vec![reference_op_to(
                vec![TEST_LEAF.to_vec()],
                b"ref",
                target.clone(),
            )],
            Some(BatchApplyOptions::default()),
            |db, gv| {
                db.get([TEST_LEAF].as_ref(), b"ref", None, gv)
                    .unwrap()
                    .expect("follow reference")
            },
            grove_version,
        );
        assert_eq!(rows, Element::new_item(vec![9]));
    }

    #[test]
    fn continuation_reference_to_item_deleted_by_initial_segment_fails() {
        // The reference target was deleted by the initial segment: the
        // continuation must fail (as one combined batch would) and commit
        // nothing.
        let grove_version = GroveVersion::latest();
        let db = ordinary_subtree(grove_version);
        let before = root_hash(&db, grove_version);

        let result = apply_partial(
            &db,
            vec![QualifiedGroveDbOp::delete_op(
                vec![TEST_LEAF.to_vec(), b"sub".to_vec()],
                0u64.to_be_bytes().to_vec(),
            )],
            vec![reference_op_to(
                vec![TEST_LEAF.to_vec()],
                b"ref",
                vec![
                    TEST_LEAF.to_vec(),
                    b"sub".to_vec(),
                    0u64.to_be_bytes().to_vec(),
                ],
            )],
            Some(BatchApplyOptions::default()),
            grove_version,
        );
        assert!(result.is_err(), "reference to a deleted target must fail");
        assert_eq!(root_hash(&db, grove_version), before, "root hash changed");
        assert_eq!(
            present_keys(&db, &[TEST_LEAF, b"sub"], grove_version),
            (0..8u64).collect::<Vec<_>>(),
            "the failed partial batch must not commit its delete"
        );
        assert_verify_clean(&db, grove_version);
    }

    // Skipped conditional inserts must carry neither their proposed value
    // nor an empty subtree placeholder across the segment boundary.
    #[test]
    fn skipped_tree_insertion_preserves_existing_children() {
        for gv in grovedb_version::version::GROVE_VERSIONS {
            for proposed_tree in [Element::empty_tree(), Element::empty_sum_tree()] {
                for pause_height in [0, 1] {
                    let db = ordinary_subtree(gv);
                    let sequential = ordinary_subtree(gv);
                    let first = vec![QualifiedGroveDbOp::insert_if_not_exists_or_skip_op(
                        vec![TEST_LEAF.to_vec()],
                        b"sub".to_vec(),
                        proposed_tree.clone(),
                    )];
                    let second = vec![item_op(vec![TEST_LEAF.to_vec(), b"sub".to_vec()], 20)];
                    sequential
                        .apply_batch(first.clone(), None, None, gv)
                        .unwrap()
                        .unwrap();
                    sequential
                        .apply_batch(second.clone(), None, None, gv)
                        .unwrap()
                        .unwrap();
                    let options = BatchApplyOptions {
                        batch_pause_height: Some(pause_height),
                        ..Default::default()
                    };
                    apply_partial(&db, first, second, Some(options), gv)
                        .expect("skipped creation followed by child insertion should succeed");
                    let mut expected: Vec<u64> = (0..8).collect();
                    expected.push(20);
                    assert_eq!(present_keys(&db, &[TEST_LEAF, b"sub"], gv), expected);
                    assert_eq!(root_hash(&db, gv), root_hash(&sequential, gv));
                    assert_verify_clean(&db, gv);
                }
            }
        }
    }

    #[test]
    fn continuation_reference_to_conditional_item_uses_actual_value() {
        for gv in grovedb_version::version::GROVE_VERSIONS {
            // The existing key skips the proposed value; the missing key
            // really inserts it. Both must resolve to the final stored value.
            for (key, expected_value) in [(0u64, 0), (20, 99)] {
                for pause_height in [0, 1] {
                    let db = ordinary_subtree(gv);
                    let first = vec![QualifiedGroveDbOp::insert_if_not_exists_or_skip_op(
                        vec![TEST_LEAF.to_vec(), b"sub".to_vec()],
                        key.to_be_bytes().to_vec(),
                        Element::new_item(vec![99]),
                    )];
                    let second = vec![reference_op_to(
                        vec![TEST_LEAF.to_vec()],
                        b"ref",
                        vec![
                            TEST_LEAF.to_vec(),
                            b"sub".to_vec(),
                            key.to_be_bytes().to_vec(),
                        ],
                    )];
                    let options = BatchApplyOptions {
                        batch_pause_height: Some(pause_height),
                        ..Default::default()
                    };
                    apply_partial(&db, first, second, Some(options), gv)
                        .expect("reference to conditional insertion should succeed");
                    assert_eq!(
                        db.get([TEST_LEAF].as_ref(), b"ref", None, gv)
                            .unwrap()
                            .unwrap(),
                        Element::new_item(vec![expected_value])
                    );
                    assert_verify_clean(&db, gv);
                }
            }
        }
    }

    #[test]
    fn continuation_reference_to_skipped_reference_uses_stored_target() {
        let gv = GroveVersion::latest();
        let db = ordinary_subtree(gv);
        let target = |key: u64| {
            vec![
                TEST_LEAF.to_vec(),
                b"sub".to_vec(),
                key.to_be_bytes().to_vec(),
            ]
        };
        db.insert(
            [TEST_LEAF, b"sub"].as_ref(),
            b"existing_ref",
            Element::new_reference(ReferencePathType::AbsolutePathReference(target(0))),
            None,
            None,
            gv,
        )
        .unwrap()
        .unwrap();
        let first = vec![QualifiedGroveDbOp::insert_if_not_exists_or_skip_op(
            vec![TEST_LEAF.to_vec(), b"sub".to_vec()],
            b"existing_ref".to_vec(),
            Element::new_reference(ReferencePathType::AbsolutePathReference(target(1))),
        )];
        let second = vec![reference_op_to(
            vec![TEST_LEAF.to_vec()],
            b"ref",
            vec![
                TEST_LEAF.to_vec(),
                b"sub".to_vec(),
                b"existing_ref".to_vec(),
            ],
        )];
        apply_partial(&db, first, second, None, gv).unwrap();
        assert_eq!(
            db.get([TEST_LEAF].as_ref(), b"ref", None, gv)
                .unwrap()
                .unwrap(),
            Element::new_item(vec![0])
        );
        assert_verify_clean(&db, gv);
    }

    #[test]
    fn skipped_indexed_tree_insertion_preserves_primary_and_secondary() {
        let gv = GroveVersion::latest();
        let db = two_group_pcit(gv);
        let sequential = two_group_pcit(gv);
        let first = vec![QualifiedGroveDbOp::insert_if_not_exists_or_skip_op(
            vec![TEST_LEAF.to_vec()],
            b"cidx".to_vec(),
            Element::empty_provable_count_indexed_tree(),
        )];
        let second = vec![bump(b"cidx", b"p", 2)];
        sequential
            .apply_batch(first.clone(), None, None, gv)
            .unwrap()
            .unwrap();
        sequential
            .apply_batch(second.clone(), None, None, gv)
            .unwrap()
            .unwrap();
        apply_partial(&db, first, second, None, gv).unwrap();
        assert_eq!(
            top_k(&db, b"cidx", gv),
            vec![(3, b"p".to_vec()), (2, b"q".to_vec())]
        );
        assert_eq!(root_hash(&db, gv), root_hash(&sequential, gv));
        assert_verify_clean(&db, gv);
    }

    /// Fresh empty trees need the same rejection as trees with child writes,
    /// before the deletion preflight attempts to read committed state.
    #[test]
    fn cross_segment_gate_rejects_deleting_or_overwriting_fresh_merk_tree() {
        for gv in grovedb_version::version::GROVE_VERSIONS {
            let path = vec![TEST_LEAF.to_vec()];
            let key = b"fresh".to_vec();
            let add_ons = [
                QualifiedGroveDbOp::delete_tree_op(
                    path.clone(),
                    key.clone(),
                    TreeType::NormalTree,
                    SubelementsDeletionBehavior::Error,
                ),
                QualifiedGroveDbOp::delete_tree_op(
                    path.clone(),
                    key.clone(),
                    TreeType::NormalTree,
                    SubelementsDeletionBehavior::Skip,
                ),
                QualifiedGroveDbOp::delete_tree_op(
                    path.clone(),
                    key.clone(),
                    TreeType::NormalTree,
                    SubelementsDeletionBehavior::DeleteChildren,
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    path.clone(),
                    key.clone(),
                    Element::new_item(vec![9]),
                ),
            ];
            for disable_operation_consistency_check in [false, true] {
                for add_on in &add_ons {
                    let db = ordinary_subtree(gv);
                    let before = root_hash(&db, gv);
                    let result = apply_partial(
                        &db,
                        vec![QualifiedGroveDbOp::insert_or_replace_op(
                            path.clone(),
                            key.clone(),
                            Element::empty_tree(),
                        )],
                        vec![add_on.clone()],
                        Some(BatchApplyOptions {
                            disable_operation_consistency_check,
                            ..Default::default()
                        }),
                        gv,
                    );
                    assert!(
                        matches!(result, Err(Error::InvalidBatchOperation(_))),
                        "{result:?}"
                    );
                    assert_eq!(root_hash(&db, gv), before);
                    assert!(db
                        .get([TEST_LEAF].as_ref(), b"fresh", None, gv)
                        .unwrap()
                        .is_err());
                    assert_verify_clean(&db, gv);
                }
            }
        }
    }

    /// Disabling per-segment checks must not permit cleanup or overwrites
    /// that strand the other segment's pending data.
    #[test]
    fn cross_segment_gate_cannot_be_disabled() {
        let gv = GroveVersion::latest();
        let sub_path = vec![TEST_LEAF.to_vec(), b"sub".to_vec()];
        let mmr_path = vec![TEST_LEAF.to_vec(), b"mmr".to_vec()];
        let cases = [
            (
                item_op(sub_path.clone(), 20),
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"sub".to_vec(),
                    Element::new_item(vec![9]),
                ),
            ),
            (
                QualifiedGroveDbOp::delete_tree_op(
                    vec![TEST_LEAF.to_vec()],
                    b"sub".to_vec(),
                    TreeType::NormalTree,
                    SubelementsDeletionBehavior::DeleteChildren,
                ),
                item_op(sub_path.clone(), 20),
            ),
            (
                item_op(sub_path, 20),
                QualifiedGroveDbOp::delete_tree_op(
                    vec![TEST_LEAF.to_vec()],
                    b"sub".to_vec(),
                    TreeType::NormalTree,
                    SubelementsDeletionBehavior::DeleteChildren,
                ),
            ),
            (
                QualifiedGroveDbOp::mmr_tree_append_op(mmr_path.clone(), b"a".to_vec()),
                QualifiedGroveDbOp::mmr_tree_append_op(mmr_path, b"b".to_vec()),
            ),
        ];
        for (first, second) in cases {
            let db = ordinary_subtree_with_mmr(gv);
            let before = root_hash(&db, gv);
            let result = apply_partial(
                &db,
                vec![first],
                vec![second],
                Some(BatchApplyOptions {
                    disable_operation_consistency_check: true,
                    ..Default::default()
                }),
                gv,
            );
            assert!(
                matches!(result, Err(Error::InvalidBatchOperation(_))),
                "{result:?}"
            );
            assert_eq!(root_hash(&db, gv), before);
            assert_eq!(
                present_keys(&db, &[TEST_LEAF, b"sub"], gv),
                (0..8u64).collect::<Vec<_>>()
            );
            assert_eq!(
                db.mmr_tree_leaf_count([TEST_LEAF].as_ref(), b"mmr", None, gv)
                    .unwrap()
                    .unwrap(),
                0
            );
            assert_verify_clean(&db, gv);
        }
    }

    /// A skipped tree insertion did not create pending state at the target
    /// and must not prevent the continuation from deleting the stored tree.
    #[test]
    fn cross_segment_gate_excludes_skipped_tree_insertions() {
        for gv in grovedb_version::version::GROVE_VERSIONS {
            let db = ordinary_subtree(gv);
            db.insert(
                [TEST_LEAF].as_ref(),
                b"empty",
                Element::empty_tree(),
                None,
                None,
                gv,
            )
            .unwrap()
            .unwrap();
            apply_partial(
                &db,
                vec![QualifiedGroveDbOp::insert_if_not_exists_or_skip_op(
                    vec![TEST_LEAF.to_vec()],
                    b"empty".to_vec(),
                    Element::empty_tree(),
                )],
                vec![QualifiedGroveDbOp::delete_tree_op(
                    vec![TEST_LEAF.to_vec()],
                    b"empty".to_vec(),
                    TreeType::NormalTree,
                    SubelementsDeletionBehavior::Error,
                )],
                None,
                gv,
            )
            .unwrap();
            assert!(db
                .get([TEST_LEAF].as_ref(), b"empty", None, gv)
                .unwrap()
                .is_err());
            assert_eq!(
                present_keys(&db, &[TEST_LEAF, b"sub"], gv),
                (0..8u64).collect::<Vec<_>>()
            );
            assert_verify_clean(&db, gv);
        }
    }
}
