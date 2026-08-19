//! Creating an indexed tree and populating it in the SAME batch.
//!
//! The pipeline opens a fresh primary and its per-axis secondaries from the
//! in-batch element (there is nothing on disk to read), and the bubble-up
//! emits `InsertAggregateIndexedTreeRootKeys` — the insert-side counterpart
//! of `ReplaceAggregateIndexedTreeRootKeys` — carrying the caller's element
//! alongside the level's computed root state.
//!
//! The load-bearing assertions here are the equivalence ones: a single batch
//! that creates and populates must reach a root hash byte-identical to the
//! split sequence (create first, populate after). That is what proves the
//! H1-A composition, the mirror, and the propagation agree with the
//! incremental path rather than merely producing *a* verifiable state.
//!
//! What stays rejected is the OVERWRITE variant: replacing an element that
//! already exists on disk with an indexed tree while writing under it in the
//! same batch — the post-apply cleanup of the old element's storage would
//! clear the new writes at the same derived prefixes.

#[cfg(test)]
mod tests {
    use grovedb_version::version::GroveVersion;

    use crate::IndexedAxisEntrySliceExt;

    use crate::IndexedAxisEntry;

    use crate::{
        batch::QualifiedGroveDbOp,
        tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
        Element, Error,
    };

    fn pcpsit_all_axes() -> Element {
        Element::empty_provable_count_provable_sum_indexed_tree(vec![
            (0u8, None),
            (1u8, None),
            (2u8, None),
        ])
        .expect("canonical axes")
    }

    fn assert_clean(db: &TempGroveDb, gv: &GroveVersion) {
        let issues = db.verify_grovedb(None, true, true, gv).expect("verify");
        assert!(issues.is_empty(), "verify_grovedb issues: {issues:?}");
    }

    fn assert_same_root(one_batch: &TempGroveDb, split: &TempGroveDb, gv: &GroveVersion) {
        assert_eq!(
            one_batch.root_hash(None, gv).unwrap().expect("root"),
            split.root_hash(None, gv).unwrap().expect("root"),
            "one-batch create+populate must reach the root hash of the split sequence"
        );
    }

    // -----------------------------------------------------------------
    // Equivalence: one batch == split batches, per variant
    // -----------------------------------------------------------------

    /// PSIT: create + two sum rows in one batch, against create-then-populate.
    #[test]
    fn fresh_psit_one_batch_equals_split_batches() {
        let gv = GroveVersion::latest();
        let create = || {
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"psit".to_vec(),
                Element::empty_provable_sum_indexed_tree(),
            )
        };
        let rows = || {
            vec![
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"psit".to_vec()],
                    b"a".to_vec(),
                    Element::new_sum_item(7),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"psit".to_vec()],
                    b"b".to_vec(),
                    Element::new_sum_item(-3),
                ),
            ]
        };

        let one = make_test_grovedb(gv);
        let mut ops = vec![create()];
        ops.extend(rows());
        one.apply_batch(ops, None, None, gv)
            .unwrap()
            .expect("one-batch create + populate");

        let split = make_test_grovedb(gv);
        split
            .apply_batch(vec![create()], None, None, gv)
            .unwrap()
            .expect("create");
        split
            .apply_batch(rows(), None, None, gv)
            .unwrap()
            .expect("populate");

        assert_same_root(&one, &split, gv);
        assert_eq!(
            one.indexed_sum_top_k([TEST_LEAF, b"psit"].as_ref(), 5, true, None, gv)
                .unwrap()
                .expect("top_k")
                .key_pairs(),
            vec![(7i64, b"a".to_vec()), (-3i64, b"b".to_vec())],
            "sum index must order the rows inserted alongside the creation"
        );
        assert_clean(&one, gv);
    }

    /// PCPSIT with all three axes: every secondary must be built from the
    /// same batch that created the primary.
    #[test]
    fn fresh_pcpsit_one_batch_equals_split_batches() {
        let gv = GroveVersion::latest();
        let create = || {
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"idx".to_vec(),
                pcpsit_all_axes(),
            )
        };
        let rows = || {
            (0..4u8)
                .map(|i| {
                    QualifiedGroveDbOp::insert_or_replace_op(
                        vec![TEST_LEAF.to_vec(), b"idx".to_vec()],
                        vec![b'k', i],
                        Element::new_item_with_sum_item(vec![b'v', i], 10 + i as i64),
                    )
                })
                .collect::<Vec<_>>()
        };

        let one = make_test_grovedb(gv);
        let mut ops = vec![create()];
        ops.extend(rows());
        one.apply_batch(ops, None, None, gv)
            .unwrap()
            .expect("one-batch create + populate");

        let split = make_test_grovedb(gv);
        split
            .apply_batch(vec![create()], None, None, gv)
            .unwrap()
            .expect("create");
        split
            .apply_batch(rows(), None, None, gv)
            .unwrap()
            .expect("populate");

        assert_same_root(&one, &split, gv);
        for (label, len) in [
            (
                "count",
                one.indexed_count_top_k([TEST_LEAF, b"idx"].as_ref(), 10, true, None, gv)
                    .unwrap()
                    .expect("count top_k")
                    .len(),
            ),
            (
                "sum",
                one.indexed_sum_top_k([TEST_LEAF, b"idx"].as_ref(), 10, true, None, gv)
                    .unwrap()
                    .expect("sum top_k")
                    .len(),
            ),
            (
                "avg",
                one.indexed_avg_top_k([TEST_LEAF, b"idx"].as_ref(), 10, true, None, gv)
                    .unwrap()
                    .expect("avg top_k")
                    .len(),
            ),
        ] {
            assert_eq!(len, 4, "{label} axis must index all four fresh rows");
        }
        assert_clean(&one, gv);
    }

    // -----------------------------------------------------------------
    // Nesting
    // -----------------------------------------------------------------

    /// The whole ancestry can be fresh: a plain tree, an indexed tree under
    /// it, and rows under that — all in one batch.
    #[test]
    fn fresh_indexed_under_fresh_plain_tree_in_one_batch() {
        let gv = GroveVersion::latest();
        let ops = || {
            vec![
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"t".to_vec(),
                    Element::empty_tree(),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"t".to_vec()],
                    b"cidx".to_vec(),
                    Element::empty_provable_count_indexed_tree(),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"t".to_vec(), b"cidx".to_vec()],
                    b"row".to_vec(),
                    Element::new_item(b"v".to_vec()),
                ),
            ]
        };

        let one = make_test_grovedb(gv);
        one.apply_batch(ops(), None, None, gv)
            .unwrap()
            .expect("fully fresh ancestry in one batch");

        let split = make_test_grovedb(gv);
        for op in ops() {
            split
                .apply_batch(vec![op], None, None, gv)
                .unwrap()
                .expect("sequential");
        }
        assert_same_root(&one, &split, gv);
        assert_eq!(
            one.indexed_count_top_k([TEST_LEAF, b"t", b"cidx"].as_ref(), 5, true, None, gv)
                .unwrap()
                .expect("top_k")
                .key_pairs(),
            vec![(1u64, b"row".to_vec())],
        );
        assert_clean(&one, gv);
    }

    /// A fresh indexed tree nested under an EXISTING indexed primary: the
    /// outer mirror must index the fresh child's derived aggregate in the
    /// same batch.
    #[test]
    fn fresh_indexed_under_existing_indexed_primary() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("outer PCIT");

        db.apply_batch(
            vec![
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"outer".to_vec()],
                    b"inner".to_vec(),
                    Element::empty_provable_count_indexed_tree(),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"outer".to_vec(), b"inner".to_vec()],
                    b"row".to_vec(),
                    Element::new_item(b"v".to_vec()),
                ),
            ],
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("fresh inner + row under existing outer");

        assert_eq!(
            one_key_count(&db, &[TEST_LEAF, b"outer"], gv).key_pairs(),
            vec![(1u64, b"inner".to_vec())],
            "the outer index must reflect the fresh inner's derived count of 1"
        );
        assert_eq!(
            one_key_count(&db, &[TEST_LEAF, b"outer", b"inner"], gv).key_pairs(),
            vec![(1u64, b"row".to_vec())],
            "the fresh inner's own index must hold its row"
        );
        assert_clean(&db, gv);
    }

    /// Both levels fresh: an indexed tree inside an indexed tree, plus a row,
    /// in one batch — two `InsertAggregateIndexedTreeRootKeys` bubbles deep.
    #[test]
    fn fresh_indexed_under_fresh_indexed_in_one_batch() {
        let gv = GroveVersion::latest();
        let ops = || {
            vec![
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"outer".to_vec(),
                    Element::empty_provable_count_indexed_tree(),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"outer".to_vec()],
                    b"inner".to_vec(),
                    Element::empty_provable_count_indexed_tree(),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"outer".to_vec(), b"inner".to_vec()],
                    b"row".to_vec(),
                    Element::new_item(b"v".to_vec()),
                ),
            ]
        };

        let one = make_test_grovedb(gv);
        one.apply_batch(ops(), None, None, gv)
            .unwrap()
            .expect("double-fresh nesting in one batch");

        let split = make_test_grovedb(gv);
        for op in ops() {
            split
                .apply_batch(vec![op], None, None, gv)
                .unwrap()
                .expect("sequential");
        }
        assert_same_root(&one, &split, gv);
        assert_clean(&one, gv);
    }

    /// A child TREE with a grandchild under a fresh indexed primary: the
    /// derived aggregate the fresh index records must come from propagation,
    /// not from the child's (empty) claim.
    #[test]
    fn fresh_indexed_with_child_tree_and_grandchild_in_one_batch() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        db.apply_batch(
            vec![
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"cidx".to_vec(),
                    Element::empty_provable_count_indexed_tree(),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                    b"child".to_vec(),
                    Element::empty_count_tree(),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"cidx".to_vec(), b"child".to_vec()],
                    b"grand".to_vec(),
                    Element::new_item(b"g".to_vec()),
                ),
            ],
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("fresh indexed + child tree + grandchild in one batch");

        assert_eq!(
            one_key_count(&db, &[TEST_LEAF, b"cidx"], gv).key_pairs(),
            vec![(1u64, b"child".to_vec())],
            "the index must record the child's PROPAGATED count of 1"
        );
        assert_clean(&db, gv);
    }

    // -----------------------------------------------------------------
    // What stays rejected
    // -----------------------------------------------------------------

    /// Overwriting an EXISTING element with an indexed tree while writing
    /// under it stays rejected: the post-apply cleanup of the old element's
    /// storage would clear the new writes.
    #[test]
    fn overwriting_existing_element_with_indexed_plus_descendants_is_rejected() {
        let gv = GroveVersion::latest();
        for (label, existing) in [
            ("plain tree", Element::empty_tree()),
            ("indexed tree", Element::empty_provable_count_indexed_tree()),
        ] {
            let db = make_test_grovedb(gv);
            db.insert([TEST_LEAF].as_ref(), b"x", existing, None, None, gv)
                .unwrap()
                .expect("existing element");
            let root_before = db.root_hash(None, gv).unwrap().expect("root");

            let err = db
                .apply_batch(
                    vec![
                        QualifiedGroveDbOp::insert_or_replace_op(
                            vec![TEST_LEAF.to_vec()],
                            b"x".to_vec(),
                            Element::empty_provable_count_indexed_tree(),
                        ),
                        QualifiedGroveDbOp::insert_or_replace_op(
                            vec![TEST_LEAF.to_vec(), b"x".to_vec()],
                            b"row".to_vec(),
                            Element::new_item(b"v".to_vec()),
                        ),
                    ],
                    None,
                    None,
                    gv,
                )
                .unwrap()
                .expect_err("overwrite + descendants must stay rejected");
            match err {
                Error::NotSupported(message) => assert!(
                    message.contains("overwriting an EXISTING element"),
                    "{label}: unexpected message: {message}"
                ),
                other => panic!("{label}: expected NotSupported, got {other:?}"),
            }
            assert_eq!(
                db.root_hash(None, gv).unwrap().expect("root"),
                root_before,
                "{label}: the refused batch must not have moved any state"
            );
        }
    }

    /// The rootless-aggregate forgery door stays shut on the fresh path: a
    /// child claiming a non-zero aggregate with no root key is refused even
    /// when its indexed parent is created in the same batch.
    #[test]
    fn fresh_path_still_rejects_rootless_aggregate_claims() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        let err = db
            .apply_batch(
                vec![
                    QualifiedGroveDbOp::insert_or_replace_op(
                        vec![TEST_LEAF.to_vec()],
                        b"cidx".to_vec(),
                        Element::empty_provable_count_indexed_tree(),
                    ),
                    QualifiedGroveDbOp::insert_or_replace_op(
                        vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                        b"forged".to_vec(),
                        Element::new_provable_count_tree_with_flags_and_count_value(None, 9, None),
                    ),
                ],
                None,
                None,
                gv,
            )
            .unwrap()
            .expect_err("a rootless count claim must be refused on the fresh path too");
        assert!(
            matches!(
                &err,
                Error::InvalidBatchOperation(message)
                    if message.contains("non-zero aggregate while having no root key")
            ),
            "unexpected error: {err:?}"
        );
        assert!(
            db.get([TEST_LEAF].as_ref(), b"cidx", None, gv)
                .unwrap()
                .is_err(),
            "the refused batch must be atomic — the creation must not have landed"
        );
    }

    fn one_key_count(
        db: &TempGroveDb,
        path: &[&[u8]],
        gv: &GroveVersion,
    ) -> Vec<IndexedAxisEntry<u64>> {
        db.indexed_count_top_k(path, 5, true, None, gv)
            .unwrap()
            .expect("count top_k")
    }
}
