//! Batch-path guards around indexed-tree primaries: which ops the
//! rootless-aggregate rule reaches, and how an overwrite of an existing indexed
//! tree is classified.
//!
//! Two separate mechanisms are pinned here.
//!
//! `capture_indexed_pre_state` matches every element-carrying `GroveOp`
//! *exhaustively* — no `_` arm — because a catch-all is how `GroveOp::Patch`
//! once slipped past the rootless-aggregate rule and forged a count that came
//! back out through top-k. Each op variant that can carry a caller-supplied
//! element therefore needs its own regression test; `Replace` is one of them.
//!
//! `classify_cidx_overwrite` runs when tree-override protection is off and
//! decides whether replacing an existing indexed tree is safe. Indexed → empty
//! indexed and indexed → non-indexed are allowed and schedule the old tree's
//! storage for cleanup; indexed → *non-empty* indexed is refused because the
//! new element's root keys would point at data the cleanup also clears.
//!
//! Its remaining branch — refusing a safe-subset overwrite that has a write
//! underneath it in the same batch — is *not* covered here, because it is
//! unreachable: the descendant level bubbles a `ReplaceTreeRootKey` into the
//! overwrite's slot before the shallower level runs, so the classifier is never
//! consulted.
//!
//! That leaves the batch orphaning the old rows. It is *not* an indexed-tree
//! defect: the identical batch against a plain `CountTree` orphans its rows the
//! same way and leaves `verify_grovedb` failing, so an indexed primary here
//! behaves exactly like every other tree type. Covering the branch would mean
//! asserting the shared pre-existing behaviour from the indexed side, which
//! would pin it in the wrong place — it belongs with the generic batch
//! tree-overwrite handling.

#[cfg(test)]
mod tests {
    use grovedb_element::indexed::IndexAxis;
    use grovedb_version::version::GroveVersion;

    use crate::{
        batch::{BatchApplyOptions, QualifiedGroveDbOp},
        tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
        Element, Error,
    };

    fn axes_tlv(axes: &[IndexAxis]) -> Vec<(u8, Option<Vec<u8>>)> {
        let mut tags: Vec<u8> = axes.iter().map(|a| a.tag()).collect();
        tags.sort_unstable();
        tags.dedup();
        tags.into_iter().map(|t| (t, None)).collect()
    }

    /// Tree-override protection off — the mode in which `classify_cidx_overwrite`
    /// is consulted at all.
    fn overwrite_allowed() -> BatchApplyOptions {
        BatchApplyOptions {
            validate_insertion_does_not_override: false,
            validate_insertion_does_not_override_tree: false,
            ..Default::default()
        }
    }

    fn assert_clean(db: &TempGroveDb, gv: &GroveVersion) {
        let issues = db.verify_grovedb(None, true, true, gv).expect("verify");
        assert!(issues.is_empty(), "verify_grovedb issues: {issues:?}");
    }

    fn make_pcit(db: &TempGroveDb, key: &[u8], gv: &GroveVersion) {
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
    }

    fn make_psit_with_entry(db: &TempGroveDb, key: &[u8], gv: &GroveVersion) {
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
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, key].as_ref(),
            b"entry",
            Element::new_sum_item(42),
            None,
            gv,
        )
        .unwrap()
        .expect("populate PSIT");
    }

    // -----------------------------------------------------------------
    // The rootless-aggregate rule reaches every element-carrying op
    // -----------------------------------------------------------------

    /// `GroveOp::Replace` carries a caller-supplied element exactly like
    /// `InsertOrReplace` and `Patch` do, so it must be held to the same rule: a
    /// child with no root key cannot assert a non-zero aggregate, because under
    /// an indexed primary that aggregate becomes the authenticated secondary
    /// sort key and the parent's contribution.
    #[test]
    fn batch_replace_cannot_forge_a_rootless_aggregate_under_an_indexed_primary() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        make_pcit(&db, b"pcit", gv);
        // A real, empty child to replace — Replace requires an existing key.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"pcit"].as_ref(),
            b"child",
            Element::empty_provable_count_tree(),
            None,
            gv,
        )
        .unwrap()
        .expect("create empty child");
        let root_before = db.root_hash(None, gv).unwrap().expect("root hash");

        let forged = vec![QualifiedGroveDbOp::replace_op(
            vec![TEST_LEAF.to_vec(), b"pcit".to_vec()],
            b"child".to_vec(),
            Element::new_provable_count_tree_with_flags_and_count_value(None, 9, None),
        )];
        let err = db
            .apply_batch(forged, None, None, gv)
            .unwrap()
            .expect_err("a Replace claiming a rootless count must be refused");
        match err {
            Error::InvalidBatchOperation(message) => assert!(
                message.contains("non-zero aggregate while having no root key"),
                "unexpected message: {message}"
            ),
            other => panic!("expected InvalidBatchOperation, got {other:?}"),
        }

        assert_eq!(
            db.root_hash(None, gv).unwrap().expect("root hash"),
            root_before,
            "the refused batch must not have moved any state"
        );
        assert_axis_entries_eq!(
            db.indexed_count_top_k([TEST_LEAF, b"pcit"].as_ref(), 5, true, None, gv)
                .unwrap()
                .expect("top_k"),
            vec![(0u64, b"child".to_vec())],
            "the child must still be indexed at its derived count of 0, not the claimed 9"
        );
        assert_clean(&db, gv);
    }

    /// `BigSumTree` is the one aggregate-bearing variant whose sum is `i128`,
    /// so it gets its own arm in the rootless-aggregate classifier. A rootless
    /// one claiming a big sum must be refused just like the `i64` variants.
    #[test]
    fn batch_refuses_a_rootless_big_sum_tree_child_of_an_indexed_primary() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        make_pcit(&db, b"pcit", gv);
        let root_before = db.root_hash(None, gv).unwrap().expect("root hash");

        let forged = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec(), b"pcit".to_vec()],
            b"big".to_vec(),
            Element::BigSumTree(None, 7, None),
        )];
        let err = db
            .apply_batch(forged, None, None, gv)
            .unwrap()
            .expect_err("a rootless BigSumTree claiming a sum must be refused");
        match err {
            Error::InvalidBatchOperation(message) => assert!(
                message.contains("non-zero aggregate while having no root key"),
                "unexpected message: {message}"
            ),
            other => panic!("expected InvalidBatchOperation, got {other:?}"),
        }
        assert_eq!(
            db.root_hash(None, gv).unwrap().expect("root hash"),
            root_before,
            "the refused batch must not have moved any state"
        );

        // The empty form is not caught by this rule (there is nothing to
        // forge), which is what makes the guard about the *claim* rather than
        // about BigSumTree children as such.
        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"pcit".to_vec()],
                b"big".to_vec(),
                Element::BigSumTree(None, 0, None),
            )],
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("an empty BigSumTree child is accepted");
        assert_clean(&db, gv);
    }

    // -----------------------------------------------------------------
    // Overwrite classification for the whole indexed family
    // -----------------------------------------------------------------

    /// Indexed → *empty* indexed of the same variant is the safe subset: the
    /// old tree's storage (subtree prefixes plus the per-axis secondary
    /// namespaces) is scheduled for cleanup, so the result must be
    /// indistinguishable from a PSIT that was never populated.
    #[test]
    fn overwriting_a_psit_with_an_empty_psit_clears_the_old_index() {
        let gv = GroveVersion::latest();

        let db = make_test_grovedb(gv);
        make_psit_with_entry(&db, b"psit", gv);
        assert_axis_entries_eq!(
            db.indexed_sum_top_k([TEST_LEAF, b"psit"].as_ref(), 5, true, None, gv)
                .unwrap()
                .expect("top_k"),
            vec![(42i64, b"entry".to_vec())],
            "baseline: the entry is indexed at sum 42"
        );

        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"psit".to_vec(),
                Element::empty_provable_sum_indexed_tree(),
            )],
            Some(overwrite_allowed()),
            None,
            gv,
        )
        .unwrap()
        .expect("indexed -> empty indexed is the safe subset");

        assert!(
            db.indexed_sum_top_k([TEST_LEAF, b"psit"].as_ref(), 5, true, None, gv)
                .unwrap()
                .expect("top_k")
                .is_empty(),
            "the old sum index must have been cleared"
        );
        assert_clean(&db, gv);

        // Byte-for-byte equal to a PSIT that was created empty and never used.
        let pristine = make_test_grovedb(gv);
        pristine
            .insert(
                [TEST_LEAF].as_ref(),
                b"psit",
                Element::empty_provable_sum_indexed_tree(),
                None,
                None,
                gv,
            )
            .unwrap()
            .expect("create empty PSIT");
        assert_eq!(
            db.root_hash(None, gv).unwrap().expect("root hash"),
            pristine.root_hash(None, gv).unwrap().expect("root hash"),
            "the overwritten PSIT must be indistinguishable from a fresh empty one"
        );
    }

    /// A PCPSIT counts as empty only when its primary root key, both aggregates
    /// *and* every axis root key are unset — the axes TLV itself stays
    /// non-empty even for an empty tree, so emptiness cannot be read off the
    /// TLV's length.
    #[test]
    fn overwriting_a_pcpsit_is_allowed_only_when_every_axis_root_key_is_unset() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        let axes = [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg];
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            Element::empty_provable_count_provable_sum_indexed_tree(axes_tlv(&axes))
                .expect("canonical axes"),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("create PCPSIT");
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"pcpsit"].as_ref(),
            b"entry",
            Element::new_item_with_sum_item(b"v".to_vec(), 8),
            None,
            gv,
        )
        .unwrap()
        .expect("populate PCPSIT");
        let root_before = db.root_hash(None, gv).unwrap().expect("root hash");

        // Non-empty replacement: one axis claims a secondary root key.
        let err = db
            .apply_batch(
                vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"pcpsit".to_vec(),
                    Element::ProvableCountProvableSumIndexedTree(
                        None,
                        0,
                        0,
                        vec![
                            (IndexAxis::Count.tag(), Some(b"claimed".to_vec())),
                            (IndexAxis::Sum.tag(), None),
                            (IndexAxis::Avg.tag(), None),
                        ],
                        None,
                    ),
                )],
                Some(overwrite_allowed()),
                None,
                gv,
            )
            .unwrap()
            .expect_err("indexed -> non-empty indexed is ambiguous and must be refused");
        // The refusal comes from the ungated empty-at-batch-insertion guard,
        // which runs before the overwrite classification ever sees the op —
        // a non-empty indexed element cannot enter a batch at all.
        match err {
            Error::InvalidBatchOperation(message) => assert!(
                message.contains("must be empty at the moment of batch insertion"),
                "unexpected message: {message}"
            ),
            other => panic!("expected InvalidBatchOperation, got {other:?}"),
        }
        assert_eq!(
            db.root_hash(None, gv).unwrap().expect("root hash"),
            root_before,
            "the refused overwrite must not have moved any state"
        );
        assert_eq!(
            db.indexed_avg_top_k([TEST_LEAF, b"pcpsit"].as_ref(), 5, true, None, gv)
                .unwrap()
                .expect("top_k")
                .len(),
            1,
            "the original entry must survive the refused overwrite"
        );

        // The same overwrite with all axis root keys unset is the safe subset.
        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"pcpsit".to_vec(),
                Element::empty_provable_count_provable_sum_indexed_tree(axes_tlv(&axes))
                    .expect("canonical axes"),
            )],
            Some(overwrite_allowed()),
            None,
            gv,
        )
        .unwrap()
        .expect("indexed -> empty indexed is the safe subset");
        for entries in [
            db.indexed_count_top_k([TEST_LEAF, b"pcpsit"].as_ref(), 5, true, None, gv)
                .unwrap()
                .expect("count top_k")
                .len(),
            db.indexed_sum_top_k([TEST_LEAF, b"pcpsit"].as_ref(), 5, true, None, gv)
                .unwrap()
                .expect("sum top_k")
                .len(),
            db.indexed_avg_top_k([TEST_LEAF, b"pcpsit"].as_ref(), 5, true, None, gv)
                .unwrap()
                .expect("avg top_k")
                .len(),
        ] {
            assert_eq!(entries, 0, "every axis secondary must have been cleared");
        }
        assert_clean(&db, gv);
    }
}
