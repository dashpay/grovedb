//! Batch writes into indexed-tree primaries, across every variant and axis.
//!
//! The batch mirror pipeline used to be single-axis by construction: it
//! captured only a count, opened exactly one secondary, and stored one
//! `(hash, root_key)` per primary. That made it PCIT-only — writes into a PSIT
//! or PCPSIT primary failed with `InvalidPath("can only propagate on tree
//! items")`, so the dedicated `insert_into_*` APIs were the only route for
//! those two variants. Since Platform composes essentially all of its state
//! transitions as batches, that made those variants close to unusable there.
//!
//! These tests pin the generalized pipeline: `(count, sum)` captured per key,
//! one secondary opened per configured axis, per-axis state carried through
//! the bubble-up, and the H1-A second input derived per variant (the lone
//! axis's root hash for PCIT/PSIT, `axes_digest` for PCPSIT).
//!
//! The most load-bearing assertions here are the equivalence ones: a batch and
//! the dedicated API must reach a byte-identical root hash for the same
//! logical write. That is what makes the two paths interchangeable rather than
//! merely both-working.

#[cfg(test)]
mod tests {
    use grovedb_element::indexed::IndexAxis;
    use grovedb_version::version::GroveVersion;

    use crate::{
        batch::QualifiedGroveDbOp,
        tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
        Element,
    };

    /// Canonical axes TLV for a PCPSIT over the given axes.
    fn axes_tlv(axes: &[IndexAxis]) -> Vec<(u8, Option<Vec<u8>>)> {
        let mut tags: Vec<u8> = axes.iter().map(|a| a.tag()).collect();
        tags.sort_unstable();
        tags.dedup();
        tags.into_iter().map(|t| (t, None)).collect()
    }

    fn make_pcpsit(db: &TempGroveDb, key: &[u8], axes: &[IndexAxis], gv: &GroveVersion) {
        db.insert(
            [TEST_LEAF].as_ref(),
            key,
            Element::empty_provable_count_provable_sum_indexed_tree(axes_tlv(axes))
                .expect("canonical axes"),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("create PCPSIT");
    }

    fn make_psit(db: &TempGroveDb, key: &[u8], gv: &GroveVersion) {
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

    fn assert_clean(db: &TempGroveDb, gv: &GroveVersion) {
        let issues = db.verify_grovedb(None, true, true, gv).expect("verify");
        assert!(issues.is_empty(), "verify_grovedb issues: {issues:?}");
    }

    // ---------------------------------------------------------------
    // Equivalence: batch and dedicated API must agree byte-for-byte
    // ---------------------------------------------------------------

    #[test]
    fn batch_and_dedicated_api_agree_on_root_hash_for_psit() {
        let gv = GroveVersion::latest();

        let via_batch = make_test_grovedb(gv);
        make_psit(&via_batch, b"psit", gv);
        via_batch
            .apply_batch(
                vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"psit".to_vec()],
                    b"a".to_vec(),
                    Element::new_sum_item(7),
                )],
                None,
                None,
                gv,
            )
            .unwrap()
            .expect("batch write into PSIT primary");

        let via_api = make_test_grovedb(gv);
        make_psit(&via_api, b"psit", gv);
        via_api
            .insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"psit"].as_ref(),
                b"a",
                Element::new_sum_item(7),
                None,
                gv,
            )
            .unwrap()
            .expect("dedicated write into PSIT primary");

        assert_eq!(
            via_batch.root_hash(None, gv).unwrap().unwrap(),
            via_api.root_hash(None, gv).unwrap().unwrap(),
            "a batch write and the dedicated API must commit the same root hash"
        );
        assert_clean(&via_batch, gv);
    }

    #[test]
    fn batch_and_dedicated_api_agree_on_root_hash_for_pcpsit_all_axes() {
        let gv = GroveVersion::latest();
        let axes = [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg];

        let via_batch = make_test_grovedb(gv);
        make_pcpsit(&via_batch, b"idx", &axes, gv);
        via_batch
            .apply_batch(
                vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"idx".to_vec()],
                    b"a".to_vec(),
                    Element::new_item_with_sum_item(b"v".to_vec(), 11),
                )],
                None,
                None,
                gv,
            )
            .unwrap()
            .expect("batch write into PCPSIT primary");

        let via_api = make_test_grovedb(gv);
        make_pcpsit(&via_api, b"idx", &axes, gv);
        via_api
            .insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"idx"].as_ref(),
                b"a",
                Element::new_item_with_sum_item(b"v".to_vec(), 11),
                None,
                gv,
            )
            .unwrap()
            .expect("dedicated write into PCPSIT primary");

        assert_eq!(
            via_batch.root_hash(None, gv).unwrap().unwrap(),
            via_api.root_hash(None, gv).unwrap().unwrap(),
            "three-axis batch and dedicated writes must commit the same root hash — if they \
             differ, the axes digest is being built from different per-axis state"
        );
        assert_clean(&via_batch, gv);
    }

    #[test]
    fn batch_and_dedicated_api_agree_on_root_hash_for_pcit() {
        // Regression: PCIT was the only variant the batch path already
        // supported, so this pins that generalizing it changed nothing.
        let gv = GroveVersion::latest();

        let via_batch = make_test_grovedb(gv);
        make_pcit(&via_batch, b"cidx", gv);
        via_batch
            .apply_batch(
                vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                    b"a".to_vec(),
                    Element::new_item(b"v".to_vec()),
                )],
                None,
                None,
                gv,
            )
            .unwrap()
            .expect("batch write into PCIT primary");

        let via_api = make_test_grovedb(gv);
        make_pcit(&via_api, b"cidx", gv);
        via_api
            .insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                b"a",
                Element::new_item(b"v".to_vec()),
                None,
                gv,
            )
            .unwrap()
            .expect("dedicated write into PCIT primary");

        assert_eq!(
            via_batch.root_hash(None, gv).unwrap().unwrap(),
            via_api.root_hash(None, gv).unwrap().unwrap(),
        );
        assert_clean(&via_batch, gv);
    }

    // ---------------------------------------------------------------
    // Every axis actually gets mirrored
    // ---------------------------------------------------------------

    #[test]
    fn batch_write_populates_all_three_pcpsit_axes() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        make_pcpsit(
            &db,
            b"idx",
            &[IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg],
            gv,
        );

        // Three entries in ONE batch, with distinct sums so each axis has a
        // distinct ordering.
        let ops: Vec<_> = [(b"a".as_slice(), 30i64), (b"b".as_slice(), 10), (b"c", 20)]
            .into_iter()
            .map(|(k, sum)| {
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"idx".to_vec()],
                    k.to_vec(),
                    Element::new_item_with_sum_item(k.to_vec(), sum),
                )
            })
            .collect();
        db.apply_batch(ops, None, None, gv)
            .unwrap()
            .expect("multi-key batch into PCPSIT");

        let path = [TEST_LEAF, b"idx"];

        // Sum axis: descending by sum.
        let by_sum = db
            .indexed_sum_top_k(path.as_ref(), 10, true, None, gv)
            .unwrap()
            .expect("sum top_k");
        assert_eq!(
            by_sum,
            vec![
                (30, b"a".to_vec()),
                (20, b"c".to_vec()),
                (10, b"b".to_vec())
            ],
            "the sum axis must be populated and ordered by sum"
        );

        // Count axis: every entry is one item, so all counts are 1 and the
        // ordering falls through to the item key.
        let by_count = db
            .indexed_count_top_k(path.as_ref(), 10, false, None, gv)
            .unwrap()
            .expect("count top_k");
        assert_eq!(
            by_count,
            vec![(1, b"a".to_vec()), (1, b"b".to_vec()), (1, b"c".to_vec())],
            "the count axis must be populated too, not just the sum axis"
        );

        // Avg axis: count is 1 for each, so avg ordering matches sum ordering.
        let by_avg = db
            .indexed_avg_top_k(path.as_ref(), 10, true, None, gv)
            .unwrap()
            .expect("avg top_k");
        let avg_keys: Vec<Vec<u8>> = by_avg.iter().map(|(_, k)| k.clone()).collect();
        assert_eq!(
            avg_keys,
            vec![b"a".to_vec(), b"c".to_vec(), b"b".to_vec()],
            "the avg axis must be populated and ordered by avg"
        );

        assert_clean(&db, gv);
    }

    #[test]
    fn batch_write_respects_configured_axis_subsets() {
        let gv = GroveVersion::latest();
        // Only the sum axis is configured; count and avg are not indexed.
        let db = make_test_grovedb(gv);
        make_pcpsit(&db, b"idx", &[IndexAxis::Sum], gv);

        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"idx".to_vec()],
                b"a".to_vec(),
                Element::new_item_with_sum_item(b"v".to_vec(), 42),
            )],
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("batch write into single-axis PCPSIT");

        let path = [TEST_LEAF, b"idx"];
        assert_eq!(
            db.indexed_sum_top_k(path.as_ref(), 10, true, None, gv)
                .unwrap()
                .expect("sum top_k"),
            vec![(42, b"a".to_vec())],
            "the configured sum axis must be populated"
        );
        // Querying an unconfigured axis must be rejected, not silently empty.
        assert!(
            db.indexed_count_top_k(path.as_ref(), 10, true, None, gv)
                .unwrap()
                .is_err(),
            "an axis this PCPSIT does not configure must not be queryable"
        );
        assert_clean(&db, gv);
    }

    // ---------------------------------------------------------------
    // Axis independence: a change can move one axis and not another
    // ---------------------------------------------------------------

    #[test]
    fn batch_update_moves_only_the_axes_whose_ordering_changed() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        make_pcpsit(
            &db,
            b"idx",
            &[IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg],
            gv,
        );
        let path = [TEST_LEAF, b"idx"];

        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"idx".to_vec()],
                b"a".to_vec(),
                Element::new_item_with_sum_item(b"v".to_vec(), 5),
            )],
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("seed");

        // Change ONLY the sum. Count stays 1, so the count axis must not move,
        // while sum and avg both must.
        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"idx".to_vec()],
                b"a".to_vec(),
                Element::new_item_with_sum_item(b"v".to_vec(), 99),
            )],
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("update sum only");

        assert_eq!(
            db.indexed_sum_top_k(path.as_ref(), 10, true, None, gv)
                .unwrap()
                .expect("sum top_k"),
            vec![(99, b"a".to_vec())],
            "the sum axis must reflect the new sum, with no stale row left behind"
        );
        assert_eq!(
            db.indexed_count_top_k(path.as_ref(), 10, true, None, gv)
                .unwrap()
                .expect("count top_k"),
            vec![(1, b"a".to_vec())],
            "the count axis must still hold exactly one row for the entry"
        );
        let by_avg = db
            .indexed_avg_top_k(path.as_ref(), 10, true, None, gv)
            .unwrap()
            .expect("avg top_k");
        assert_eq!(by_avg.len(), 1, "the avg axis must not have a stale row");
        assert_eq!(by_avg[0].1, b"a".to_vec());

        assert_clean(&db, gv);
    }

    // ---------------------------------------------------------------
    // Deletes
    // ---------------------------------------------------------------

    #[test]
    fn batch_delete_removes_the_entry_from_every_axis() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        make_pcpsit(
            &db,
            b"idx",
            &[IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg],
            gv,
        );
        let path = [TEST_LEAF, b"idx"];

        let ops: Vec<_> = [(b"a".as_slice(), 5i64), (b"b".as_slice(), 8)]
            .into_iter()
            .map(|(k, sum)| {
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"idx".to_vec()],
                    k.to_vec(),
                    Element::new_item_with_sum_item(k.to_vec(), sum),
                )
            })
            .collect();
        db.apply_batch(ops, None, None, gv).unwrap().expect("seed");

        db.apply_batch(
            vec![QualifiedGroveDbOp::delete_op(
                vec![TEST_LEAF.to_vec(), b"idx".to_vec()],
                b"a".to_vec(),
            )],
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("batch delete from PCPSIT primary");

        for (label, got) in [
            (
                "sum",
                db.indexed_sum_top_k(path.as_ref(), 10, true, None, gv)
                    .unwrap()
                    .expect("sum top_k")
                    .into_iter()
                    .map(|(_, k)| k)
                    .collect::<Vec<_>>(),
            ),
            (
                "count",
                db.indexed_count_top_k(path.as_ref(), 10, true, None, gv)
                    .unwrap()
                    .expect("count top_k")
                    .into_iter()
                    .map(|(_, k)| k)
                    .collect::<Vec<_>>(),
            ),
            (
                "avg",
                db.indexed_avg_top_k(path.as_ref(), 10, true, None, gv)
                    .unwrap()
                    .expect("avg top_k")
                    .into_iter()
                    .map(|(_, k)| k)
                    .collect::<Vec<_>>(),
            ),
        ] {
            assert_eq!(
                got,
                vec![b"b".to_vec()],
                "the {label} axis must have dropped the deleted entry and kept the other"
            );
        }

        assert_clean(&db, gv);
    }

    #[test]
    fn batch_delete_from_psit_primary_updates_the_sum_axis() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        make_psit(&db, b"psit", gv);

        db.apply_batch(
            vec![
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"psit".to_vec()],
                    b"a".to_vec(),
                    Element::new_sum_item(3),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"psit".to_vec()],
                    b"b".to_vec(),
                    Element::new_sum_item(4),
                ),
            ],
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("seed PSIT");

        db.apply_batch(
            vec![QualifiedGroveDbOp::delete_op(
                vec![TEST_LEAF.to_vec(), b"psit".to_vec()],
                b"b".to_vec(),
            )],
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("batch delete from PSIT");

        assert_eq!(
            db.indexed_sum_top_k([TEST_LEAF, b"psit"].as_ref(), 10, true, None, gv)
                .unwrap()
                .expect("sum top_k"),
            vec![(3, b"a".to_vec())],
        );
        assert_clean(&db, gv);
    }

    // ---------------------------------------------------------------
    // Mixed batches and negative sums
    // ---------------------------------------------------------------

    #[test]
    fn one_batch_can_touch_several_indexed_trees_of_different_variants() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        make_pcit(&db, b"cidx", gv);
        make_psit(&db, b"psit", gv);
        make_pcpsit(&db, b"idx", &[IndexAxis::Count, IndexAxis::Sum], gv);

        db.apply_batch(
            vec![
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                    b"x".to_vec(),
                    Element::new_item(b"v".to_vec()),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"psit".to_vec()],
                    b"y".to_vec(),
                    Element::new_sum_item(12),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"idx".to_vec()],
                    b"z".to_vec(),
                    Element::new_item_with_sum_item(b"z".to_vec(), 6),
                ),
            ],
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("one batch spanning three indexed variants");

        assert_eq!(
            db.indexed_count_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, gv)
                .unwrap()
                .expect("pcit")
                .len(),
            1
        );
        assert_eq!(
            db.indexed_sum_top_k([TEST_LEAF, b"psit"].as_ref(), 10, true, None, gv)
                .unwrap()
                .expect("psit"),
            vec![(12, b"y".to_vec())]
        );
        assert_eq!(
            db.indexed_sum_top_k([TEST_LEAF, b"idx"].as_ref(), 10, true, None, gv)
                .unwrap()
                .expect("pcpsit sum"),
            vec![(6, b"z".to_vec())]
        );
        assert_clean(&db, gv);
    }

    #[test]
    fn batch_handles_negative_sums_across_axes() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        make_pcpsit(&db, b"idx", &[IndexAxis::Sum, IndexAxis::Avg], gv);

        let ops: Vec<_> = [
            (b"neg".as_slice(), -50i64),
            (b"zero".as_slice(), 0),
            (b"pos".as_slice(), 50),
        ]
        .into_iter()
        .map(|(k, sum)| {
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"idx".to_vec()],
                k.to_vec(),
                Element::new_item_with_sum_item(k.to_vec(), sum),
            )
        })
        .collect();
        db.apply_batch(ops, None, None, gv)
            .unwrap()
            .expect("batch with negative sums");

        assert_eq!(
            db.indexed_sum_top_k([TEST_LEAF, b"idx"].as_ref(), 10, false, None, gv)
                .unwrap()
                .expect("ascending sum top_k"),
            vec![
                (-50, b"neg".to_vec()),
                (0, b"zero".to_vec()),
                (50, b"pos".to_vec())
            ],
            "the sign-flipped sum encoding must order negatives below zero"
        );
        assert_clean(&db, gv);
    }

    // ---------------------------------------------------------------
    // Nesting
    // ---------------------------------------------------------------

    #[test]
    fn batch_write_into_a_pcpsit_nested_below_a_plain_tree() {
        // Exercises bubble-up across an intermediate level: the PCPSIT's
        // per-axis state has to reach the element two levels up.
        //
        // (Nesting an indexed tree directly inside ANOTHER indexed tree is a
        // separate, still-unsupported case — the dedicated insert rejects it
        // as Phase 2 — so this uses a plain tree as the intermediate.)
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"mid",
            Element::empty_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("create intermediate tree");
        db.insert(
            [TEST_LEAF, b"mid"].as_ref(),
            b"idx",
            Element::empty_provable_count_provable_sum_indexed_tree(axes_tlv(&[
                IndexAxis::Count,
                IndexAxis::Sum,
            ]))
            .expect("canonical axes"),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("nest a PCPSIT below a plain tree");

        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"mid".to_vec(), b"idx".to_vec()],
                b"a".to_vec(),
                Element::new_item_with_sum_item(b"v".to_vec(), 15),
            )],
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("batch write into the nested PCPSIT");

        assert_eq!(
            db.indexed_sum_top_k([TEST_LEAF, b"mid", b"idx"].as_ref(), 10, true, None, gv)
                .unwrap()
                .expect("nested sum top_k"),
            vec![(15, b"a".to_vec())],
            "the nested PCPSIT's sum axis must be mirrored through the intermediate level"
        );
        assert_eq!(
            db.indexed_count_top_k([TEST_LEAF, b"mid", b"idx"].as_ref(), 10, true, None, gv)
                .unwrap()
                .expect("nested count top_k"),
            vec![(1, b"a".to_vec())],
        );
        assert_clean(&db, gv);
    }

    // ---------------------------------------------------------------
    // Ordering determinism
    // ---------------------------------------------------------------

    #[test]
    fn batch_write_order_does_not_affect_the_root_hash() {
        let gv = GroveVersion::latest();
        let entries: [(&[u8], i64); 4] = [(b"d", 4), (b"a", 1), (b"c", 3), (b"b", 2)];

        let build = |order: &[usize]| {
            let db = make_test_grovedb(gv);
            make_pcpsit(
                &db,
                b"idx",
                &[IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg],
                gv,
            );
            let ops: Vec<_> = order
                .iter()
                .map(|i| {
                    let (k, sum) = entries[*i];
                    QualifiedGroveDbOp::insert_or_replace_op(
                        vec![TEST_LEAF.to_vec(), b"idx".to_vec()],
                        k.to_vec(),
                        Element::new_item_with_sum_item(k.to_vec(), sum),
                    )
                })
                .collect();
            db.apply_batch(ops, None, None, gv).unwrap().expect("batch");
            db.root_hash(None, gv).unwrap().unwrap()
        };

        assert_eq!(
            build(&[0, 1, 2, 3]),
            build(&[3, 2, 1, 0]),
            "the mirror assembles a sorted batch per axis, so op order must not \
             change the committed root"
        );
    }

    // ---------------------------------------------------------------
    // Per-axis item-key ceiling
    // ---------------------------------------------------------------

    #[test]
    fn item_key_ceiling_tightens_when_the_avg_axis_is_configured() {
        // The secondary key is sort_key ‖ item_key and Merk requires < 256
        // bytes. Count and sum prepend 8 bytes (247 allowed); avg prepends 16
        // (239 allowed). The bound must follow the widest CONFIGURED axis, so
        // the same 243-byte key is fine without avg and rejected with it.
        let gv = GroveVersion::latest();
        let key_243 = vec![b'k'; 243];

        let without_avg = make_test_grovedb(gv);
        make_pcpsit(
            &without_avg,
            b"idx",
            &[IndexAxis::Count, IndexAxis::Sum],
            gv,
        );
        without_avg
            .apply_batch(
                vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"idx".to_vec()],
                    key_243.clone(),
                    Element::new_item_with_sum_item(b"v".to_vec(), 1),
                )],
                None,
                None,
                gv,
            )
            .unwrap()
            .expect("243 bytes fits under an 8-byte sort key");
        assert_clean(&without_avg, gv);

        let with_avg = make_test_grovedb(gv);
        make_pcpsit(
            &with_avg,
            b"idx",
            &[IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg],
            gv,
        );
        let rejected = with_avg
            .apply_batch(
                vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"idx".to_vec()],
                    key_243,
                    Element::new_item_with_sum_item(b"v".to_vec(), 1),
                )],
                None,
                None,
                gv,
            )
            .unwrap();
        assert!(
            matches!(&rejected, Err(crate::Error::InvalidInput(m)) if m.contains("too long for a configured axis")),
            "with the avg axis configured the same key must be rejected, got {rejected:?}"
        );
    }

    // ---------------------------------------------------------------
    // Other op shapes
    // ---------------------------------------------------------------

    #[test]
    fn insert_if_not_exists_into_an_indexed_primary_mirrors_every_axis() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        make_pcpsit(&db, b"idx", &[IndexAxis::Count, IndexAxis::Sum], gv);

        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_if_not_exists_op(
                vec![TEST_LEAF.to_vec(), b"idx".to_vec()],
                b"a".to_vec(),
                Element::new_item_with_sum_item(b"v".to_vec(), 21),
            )],
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("insert_if_not_exists into an indexed primary");

        assert_eq!(
            db.indexed_sum_top_k([TEST_LEAF, b"idx"].as_ref(), 10, true, None, gv)
                .unwrap()
                .expect("sum top_k"),
            vec![(21, b"a".to_vec())],
            "an InsertIfNotExists op must mirror like an insert — it carries an element too"
        );
        assert_clean(&db, gv);
    }

    // ---------------------------------------------------------------
    // Volume
    // ---------------------------------------------------------------

    #[test]
    fn a_single_batch_of_many_keys_mirrors_all_axes_consistently() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        make_pcpsit(
            &db,
            b"idx",
            &[IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg],
            gv,
        );

        // 50 entries with distinct, deliberately unsorted sums.
        let n = 50u8;
        let ops: Vec<_> = (0..n)
            .map(|i| {
                let sum = ((i as i64) * 37) % 101 - 50;
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"idx".to_vec()],
                    vec![b'k', i],
                    Element::new_item_with_sum_item(vec![i], sum),
                )
            })
            .collect();
        db.apply_batch(ops, None, None, gv)
            .unwrap()
            .expect("50-key batch");

        let path = [TEST_LEAF, b"idx"];
        let by_sum = db
            .indexed_sum_top_k(path.as_ref(), 100, false, None, gv)
            .unwrap()
            .expect("sum top_k");
        assert_eq!(by_sum.len(), n as usize, "every entry must be indexed");
        let sums: Vec<i64> = by_sum.iter().map(|(s, _)| *s).collect();
        let mut sorted = sums.clone();
        sorted.sort_unstable();
        assert_eq!(sums, sorted, "the sum axis must come back ascending");

        assert_eq!(
            db.indexed_count_top_k(path.as_ref(), 100, false, None, gv)
                .unwrap()
                .expect("count top_k")
                .len(),
            n as usize,
            "the count axis must hold the same number of entries as the sum axis"
        );
        assert_eq!(
            db.indexed_avg_top_k(path.as_ref(), 100, false, None, gv)
                .unwrap()
                .expect("avg top_k")
                .len(),
            n as usize,
            "and so must the avg axis — a partially-mirrored axis is the failure this catches"
        );
        assert_clean(&db, gv);
    }

    // ---------------------------------------------------------------
    // Delete equivalence: the dedicated delete now delegates to batch
    // ---------------------------------------------------------------

    /// Seed two entries, remove one, and require the batch and dedicated
    /// doors to land on the same root — the delete-side counterpart of the
    /// insert equivalence tests, and what makes delegating the dedicated
    /// delete safe.
    fn assert_delete_equivalence(
        seed: impl Fn(&TempGroveDb, &GroveVersion),
        via_batch_delete: impl Fn(&TempGroveDb, &GroveVersion),
        via_api_delete: impl Fn(&TempGroveDb, &GroveVersion),
        gv: &GroveVersion,
    ) {
        let a = make_test_grovedb(gv);
        seed(&a, gv);
        via_batch_delete(&a, gv);

        let b = make_test_grovedb(gv);
        seed(&b, gv);
        via_api_delete(&b, gv);

        assert_eq!(
            a.root_hash(None, gv).unwrap().unwrap(),
            b.root_hash(None, gv).unwrap().unwrap(),
            "a batch delete and the dedicated delete must commit the same root hash"
        );
        assert_clean(&a, gv);
        assert_clean(&b, gv);
    }

    #[test]
    fn batch_and_dedicated_delete_agree_for_pcit() {
        let gv = GroveVersion::latest();
        assert_delete_equivalence(
            |db, gv| {
                make_pcit(db, b"cidx", gv);
                for k in [b"a".as_slice(), b"b".as_slice()] {
                    db.insert_into_count_indexed_tree(
                        [TEST_LEAF, b"cidx"].as_ref(),
                        k,
                        Element::new_item(k.to_vec()),
                        None,
                        gv,
                    )
                    .unwrap()
                    .expect("seed");
                }
            },
            |db, gv| {
                db.apply_batch(
                    vec![QualifiedGroveDbOp::delete_op(
                        vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                        b"a".to_vec(),
                    )],
                    None,
                    None,
                    gv,
                )
                .unwrap()
                .expect("batch delete");
            },
            |db, gv| {
                assert!(
                    db.delete_from_count_indexed_tree(
                        [TEST_LEAF, b"cidx"].as_ref(),
                        b"a",
                        None,
                        gv
                    )
                    .unwrap()
                    .expect("dedicated delete"),
                    "deleting an existing entry must report true"
                );
            },
            gv,
        );
    }

    #[test]
    fn batch_and_dedicated_delete_agree_for_pcpsit_all_axes() {
        let gv = GroveVersion::latest();
        let axes = [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg];
        assert_delete_equivalence(
            move |db, gv| {
                make_pcpsit(db, b"idx", &axes, gv);
                for (k, sum) in [(b"a".as_slice(), 5i64), (b"b".as_slice(), 9)] {
                    db.insert_into_provable_count_provable_sum_indexed_tree(
                        [TEST_LEAF, b"idx"].as_ref(),
                        k,
                        Element::new_item_with_sum_item(k.to_vec(), sum),
                        None,
                        gv,
                    )
                    .unwrap()
                    .expect("seed");
                }
            },
            |db, gv| {
                db.apply_batch(
                    vec![QualifiedGroveDbOp::delete_op(
                        vec![TEST_LEAF.to_vec(), b"idx".to_vec()],
                        b"a".to_vec(),
                    )],
                    None,
                    None,
                    gv,
                )
                .unwrap()
                .expect("batch delete");
            },
            |db, gv| {
                assert!(db
                    .delete_from_provable_count_provable_sum_indexed_tree(
                        [TEST_LEAF, b"idx"].as_ref(),
                        b"a",
                        None,
                        gv
                    )
                    .unwrap()
                    .expect("dedicated delete"),);
            },
            gv,
        );
    }

    #[test]
    fn dedicated_delete_of_an_absent_key_is_a_no_op_returning_false() {
        // The batch op carries no notion of "was anything removed", so the
        // wrapper probes first. An absent key must not apply an empty batch,
        // which would still move nothing but is worth pinning explicitly.
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        make_psit(&db, b"psit", gv);
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"a",
            Element::new_sum_item(4),
            None,
            gv,
        )
        .unwrap()
        .expect("seed");

        let before = db.root_hash(None, gv).unwrap().unwrap();
        let removed = db
            .delete_from_provable_sum_indexed_tree(
                [TEST_LEAF, b"psit"].as_ref(),
                b"missing",
                None,
                gv,
            )
            .unwrap()
            .expect("deleting an absent key must not error");
        assert!(!removed, "an absent key must report false");
        assert_eq!(
            db.root_hash(None, gv).unwrap().unwrap(),
            before,
            "a no-op delete must not move the root"
        );
        assert_clean(&db, gv);
    }

    #[test]
    fn dedicated_delete_rejects_a_mismatched_variant() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        make_psit(&db, b"psit", gv);
        let result = db
            .delete_from_count_indexed_tree([TEST_LEAF, b"psit"].as_ref(), b"a", None, gv)
            .unwrap();
        assert!(
            matches!(&result, Err(crate::Error::InvalidPath(m)) if m.contains("must be a")),
            "the count-indexed delete must refuse a PSIT target, got {result:?}"
        );
    }
}
