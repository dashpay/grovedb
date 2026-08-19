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
        assert_axis_entries_eq!(
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
        assert_axis_entries_eq!(
            by_count,
            vec![(1, b"a".to_vec()), (1, b"b".to_vec()), (1, b"c".to_vec())],
            "the count axis must be populated too, not just the sum axis"
        );

        // Avg axis: count is 1 for each, so avg ordering matches sum ordering.
        let by_avg = db
            .indexed_avg_top_k(path.as_ref(), 10, true, None, gv)
            .unwrap()
            .expect("avg top_k");
        let avg_keys: Vec<Vec<u8>> = by_avg
            .iter()
            .map(|entry| entry.primary_key.clone())
            .collect();
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
        assert_axis_entries_eq!(
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

        assert_axis_entries_eq!(
            db.indexed_sum_top_k(path.as_ref(), 10, true, None, gv)
                .unwrap()
                .expect("sum top_k"),
            vec![(99, b"a".to_vec())],
            "the sum axis must reflect the new sum, with no stale row left behind"
        );
        assert_axis_entries_eq!(
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
        assert_eq!(by_avg[0].primary_key, b"a".to_vec());

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
                    .map(|entry| entry.primary_key)
                    .collect::<Vec<_>>(),
            ),
            (
                "count",
                db.indexed_count_top_k(path.as_ref(), 10, true, None, gv)
                    .unwrap()
                    .expect("count top_k")
                    .into_iter()
                    .map(|entry| entry.primary_key)
                    .collect::<Vec<_>>(),
            ),
            (
                "avg",
                db.indexed_avg_top_k(path.as_ref(), 10, true, None, gv)
                    .unwrap()
                    .expect("avg top_k")
                    .into_iter()
                    .map(|entry| entry.primary_key)
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

        assert_axis_entries_eq!(
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
        assert_axis_entries_eq!(
            db.indexed_sum_top_k([TEST_LEAF, b"psit"].as_ref(), 10, true, None, gv)
                .unwrap()
                .expect("psit"),
            vec![(12, b"y".to_vec())]
        );
        assert_axis_entries_eq!(
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

        assert_axis_entries_eq!(
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

        assert_axis_entries_eq!(
            db.indexed_sum_top_k([TEST_LEAF, b"mid", b"idx"].as_ref(), 10, true, None, gv)
                .unwrap()
                .expect("nested sum top_k"),
            vec![(15, b"a".to_vec())],
            "the nested PCPSIT's sum axis must be mirrored through the intermediate level"
        );
        assert_axis_entries_eq!(
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

        assert_axis_entries_eq!(
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
        let sums: Vec<i64> = by_sum.iter().map(|entry| entry.ordering_value).collect();
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

    // ---------------------------------------------------------------
    // Storage-cleanup regressions from the consolidation
    // ---------------------------------------------------------------

    /// Deleting a TREE-typed child must sweep the subtree it owned.
    ///
    /// `GroveOp::Delete` only unlinks the entry; the batch path's recursive
    /// storage cleanup is driven by `DeleteTree`. Emitting a plain delete left
    /// the child's rows at its derived prefix, and since prefixes are
    /// path-derived, re-creating at the same key inherited them — `db.query`
    /// returns them via raw iteration and `verify_grovedb` rejects them as
    /// data the Merk cannot attest to.
    #[test]
    fn deleting_a_tree_child_sweeps_its_storage() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        make_pcit(&db, b"cidx", gv);
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::empty_count_tree(),
            None,
            gv,
        )
        .unwrap()
        .expect("insert tree child");
        db.insert(
            [TEST_LEAF, b"cidx", b"a"].as_ref(),
            b"seed",
            Element::new_item(b"ghost".to_vec()),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("populate child");

        assert!(
            db.delete_from_count_indexed_tree([TEST_LEAF, b"cidx"].as_ref(), b"a", None, gv)
                .unwrap()
                .expect("delete child"),
            "deleting an existing child reports true"
        );

        // Re-create at the same key: the namespace must be empty, not haunted.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::empty_count_tree(),
            None,
            gv,
        )
        .unwrap()
        .expect("re-create child");
        assert!(
            db.verify_grovedb(None, true, true, gv)
                .expect("verify must not hard-error on resurrected rows")
                .is_empty(),
            "the re-created child must not inherit the deleted child's rows"
        );
        assert!(
            db.get([TEST_LEAF, b"cidx", b"a"].as_ref(), b"seed", None, gv)
                .unwrap()
                .is_err(),
            "the old row must be gone, not merely unlinked"
        );
    }

    /// Overwriting a populated TREE-typed child must sweep it too — same
    /// hazard as delete, reached without a delete/recreate cycle.
    #[test]
    fn overwriting_a_populated_tree_child_sweeps_its_storage() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        make_pcit(&db, b"cidx", gv);
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::empty_count_tree(),
            None,
            gv,
        )
        .unwrap()
        .expect("insert tree child");
        db.insert(
            [TEST_LEAF, b"cidx", b"a"].as_ref(),
            b"seed",
            Element::new_item(b"ghost".to_vec()),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("populate child");

        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::empty_count_tree(),
            None,
            gv,
        )
        .unwrap()
        .expect("overwrite with a fresh empty tree");

        assert!(
            db.verify_grovedb(None, true, true, gv)
                .expect("verify must not hard-error")
                .is_empty(),
            "the replacement must not inherit the overwritten child's rows"
        );
    }

    /// An avg-axis change that alters the payload but NOT the sort key must
    /// not emit a delete and a put for one key.
    ///
    /// `(count, sum)` going (1, 5) -> (2, 10) keeps avg = 5, so the row stays
    /// at the same secondary key while its stored sum changes. Emitting both
    /// operations put a duplicate key in one Merk batch, which merk rejects
    /// outright — failing the entire GroveDB batch.
    #[test]
    fn avg_axis_payload_change_at_a_fixed_sort_key_succeeds() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        make_pcpsit(
            &db,
            b"idx",
            &[IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg],
            gv,
        );
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"idx"].as_ref(),
            b"k",
            Element::empty_count_sum_tree(),
            None,
            gv,
        )
        .unwrap()
        .expect("empty child");
        db.insert(
            [TEST_LEAF, b"idx", b"k"].as_ref(),
            b"a",
            Element::new_sum_item(5),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("child becomes (count 1, sum 5)");

        let avg_before = db
            .indexed_avg_top_k([TEST_LEAF, b"idx"].as_ref(), 5, true, None, gv)
            .unwrap()
            .expect("avg top_k");

        // (1, 5) -> (2, 10): avg is unchanged, the payload is not.
        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"idx".to_vec(), b"k".to_vec()],
                b"b".to_vec(),
                Element::new_sum_item(5),
            )],
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("a payload-only change on the avg axis must not fail the batch");

        let avg_after = db
            .indexed_avg_top_k([TEST_LEAF, b"idx"].as_ref(), 5, true, None, gv)
            .unwrap()
            .expect("avg top_k");
        assert_eq!(
            avg_before[0].ordering_value, avg_after[0].ordering_value,
            "the avg sort key is unchanged by (1,5) -> (2,10)"
        );
        assert_eq!(avg_before[0].primary_key, avg_after[0].primary_key);
        assert_ne!(
            avg_before[0].value, avg_after[0].value,
            "the canonical reference row must resolve the refreshed primary value"
        );
        assert!(matches!(
            avg_after[0].value,
            Element::CountSumTree(_, 2, 10, _)
        ));
        assert_axis_entries_eq!(
            db.indexed_sum_top_k([TEST_LEAF, b"idx"].as_ref(), 5, true, None, gv)
                .unwrap()
                .expect("sum top_k"),
            vec![(10, b"k".to_vec())],
            "the sum axis must reflect the new total"
        );
        assert_clean(&db, gv);
    }

    /// The direct write path and the batch path must commit the SAME
    /// secondary shape for the same logical mutation — root hashes are
    /// consensus.
    ///
    /// The one transition where the two used to diverge: an avg-axis
    /// payload change at a fixed sort key ((1, 5) -> (2, 10) keeps
    /// avg = 5). The batch mirror emits a single replacement write; the
    /// direct path used to DELETE and REINSERT, which rebalances twice
    /// and can settle a different AVL shape in the avg secondary —
    /// identical data, different secondary root, different grove root.
    #[test]
    fn direct_and_batch_agree_on_root_for_a_fixed_key_avg_payload_change() {
        let gv = GroveVersion::latest();

        // Identical seeding on both databases: enough avg-secondary
        // neighbors that a delete+reinsert has room to change shape.
        let build = || {
            let db = make_test_grovedb(gv);
            make_pcpsit(
                &db,
                b"idx",
                &[IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg],
                gv,
            );
            // 24 children with distinct sums straddling k0's avg on
            // both sides, inserted in an order that leaves the avg
            // secondary deep enough for a delete+reinsert to have room
            // to settle a different shape.
            let mut seeds: Vec<(Vec<u8>, i64)> = (0..24i64)
                .map(|i| (format!("k{:02}", i).into_bytes(), 2 * i + 5))
                .collect();
            seeds.swap(3, 20);
            seeds.swap(7, 15);
            for (child, sum) in [(b"k0".to_vec(), 29i64)]
                .into_iter()
                .chain(seeds.into_iter())
            {
                let child = child.as_slice();
                db.insert_into_provable_count_provable_sum_indexed_tree(
                    [TEST_LEAF, b"idx"].as_ref(),
                    child,
                    Element::empty_count_sum_tree(),
                    None,
                    gv,
                )
                .unwrap()
                .expect("empty child");
                db.insert(
                    [TEST_LEAF, b"idx", child].as_ref(),
                    b"a",
                    Element::new_sum_item(sum),
                    None,
                    None,
                    gv,
                )
                .unwrap()
                .expect("seed child");
            }
            db
        };

        // (1, 29) -> (2, 58) on k0: avg stays 29 — mid-range among the
        // neighbors' 5..=53, so k0's node sits in the interior of the
        // avg secondary where a delete visibly rebalances.
        let direct = build();
        direct
            .insert(
                [TEST_LEAF, b"idx", b"k0"].as_ref(),
                b"b",
                Element::new_sum_item(29),
                None,
                None,
                gv,
            )
            .unwrap()
            .expect("direct proportional change");

        let batched = build();
        batched
            .apply_batch(
                vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"idx".to_vec(), b"k0".to_vec()],
                    b"b".to_vec(),
                    Element::new_sum_item(29),
                )],
                None,
                None,
                gv,
            )
            .unwrap()
            .expect("batched proportional change");

        assert_clean(&direct, gv);
        assert_clean(&batched, gv);
        assert_eq!(
            direct.root_hash(None, gv).unwrap().expect("direct root"),
            batched.root_hash(None, gv).unwrap().expect("batched root"),
            "the two write paths must commit identical roots for identical data"
        );
    }

    /// Every non-Merk tree type survives a full insert/delete cycle as a
    /// child of an indexed primary.
    ///
    /// PCIT places no shape restriction on its children, so all four
    /// append-style tree types can land under one. They matter here because
    /// the delete path passes the child's ACTUAL stored tree type to
    /// `delete_tree_op`, which is what selects the cleanup namespaces — the
    /// behaviour GROVE_V4's `delete_tree_cleanup_type_source` gate turns on.
    /// A type the sweep mishandles would orphan storage exactly the way a
    /// plain `Delete` did, so each variant is verified, not just accepted.
    #[test]
    fn every_non_merk_tree_type_round_trips_as_an_indexed_child() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        make_pcit(&db, b"cidx", gv);

        for (label, element) in [
            ("mmr", Element::empty_mmr_tree()),
            (
                "commitment",
                Element::empty_commitment_tree(4).expect("chunk_power 4 is in range"),
            ),
            ("dense", Element::empty_dense_tree(4)),
            (
                "bulk",
                Element::empty_bulk_append_tree(4).expect("chunk_power 4 is in range"),
            ),
        ] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                label.as_bytes(),
                element,
                None,
                gv,
            )
            .unwrap()
            .unwrap_or_else(|e| panic!("{label} must be insertable under a PCIT: {e}"));

            assert!(
                db.delete_from_count_indexed_tree(
                    [TEST_LEAF, b"cidx"].as_ref(),
                    label.as_bytes(),
                    None,
                    gv,
                )
                .unwrap()
                .unwrap_or_else(|e| panic!("{label} must be deletable from a PCIT: {e}")),
                "{label}: deleting an existing child reports true"
            );

            assert!(
                db.verify_grovedb(None, true, true, gv)
                    .unwrap_or_else(|e| panic!("{label}: verify hard-errored: {e}"))
                    .is_empty(),
                "{label}: the sweep must leave no orphaned rows behind"
            );
        }
    }

    /// Every direct non-Merk append rewrites its parent element's commitment
    /// without changing the PCIT ordering value. The canonical reference row
    /// must therefore be refreshed in place for all four APIs.
    #[test]
    fn every_direct_non_merk_append_refreshes_its_indexed_row() {
        let gv = GroveVersion::latest();

        let pcit_with_child = |key: &[u8], child: Element| {
            let db = make_test_grovedb(gv);
            make_pcit(&db, b"cidx", gv);
            db.insert_into_count_indexed_tree([TEST_LEAF, b"cidx"].as_ref(), key, child, None, gv)
                .unwrap()
                .expect("insert non-Merk indexed child");
            db
        };

        let db = pcit_with_child(b"mmr", Element::empty_mmr_tree());
        db.mmr_tree_append(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"mmr",
            b"leaf".to_vec(),
            None,
            gv,
        )
        .unwrap()
        .expect("MMR append");
        assert_clean(&db, gv);

        let db = pcit_with_child(
            b"bulk",
            Element::empty_bulk_append_tree(4).expect("bulk tree"),
        );
        db.bulk_append(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"bulk",
            b"value".to_vec(),
            None,
            gv,
        )
        .unwrap()
        .expect("bulk append");
        assert_clean(&db, gv);

        let db = pcit_with_child(b"dense", Element::empty_dense_tree(4));
        db.dense_tree_insert(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"dense",
            b"value".to_vec(),
            None,
            gv,
        )
        .unwrap()
        .expect("dense insert");
        assert_clean(&db, gv);

        let db = pcit_with_child(
            b"commitment",
            Element::empty_commitment_tree(4).expect("commitment tree"),
        );
        db.commitment_tree_insert_raw(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"commitment",
            [1u8; 32],
            [2u8; 32],
            [3u8; 32],
            vec![0u8; 216],
            None,
            gv,
        )
        .unwrap()
        .expect("commitment insert");
        assert_clean(&db, gv);
    }

    /// A deep write *under* a child of an indexed primary keeps the secondary
    /// in sync on its own, through both the generic and the batch path.
    ///
    /// This is the scenario `reconcile_indexed_tree_secondaries` was
    /// written for: inserting into a sub-`CountTree` held inside a PCIT
    /// propagates the sub-tree's aggregate up into the primary's element
    /// bytes, which is the value the secondary orders by. It used to desync
    /// because neither path knew about the secondary. Both now maintain it,
    /// which is what makes reconcile a repair-only API rather than a step
    /// callers have to remember — so if this ever regresses, the failure is
    /// silent index corruption and reconcile's doc needs revisiting too.
    #[test]
    fn a_deep_write_under_an_indexed_child_keeps_the_secondary_in_sync() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        make_pcit(&db, b"cidx", gv);
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"child",
            Element::empty_count_tree(),
            None,
            gv,
        )
        .unwrap()
        .expect("child count tree");
        assert_axis_entries_eq!(
            db.indexed_count_top_k([TEST_LEAF, b"cidx"].as_ref(), 5, true, None, gv)
                .unwrap()
                .expect("top_k"),
            vec![(0u64, b"child".to_vec())],
            "baseline: an empty child is indexed at count 0"
        );

        db.insert(
            [TEST_LEAF, b"cidx", b"child"].as_ref(),
            b"a",
            Element::new_item(b"v".to_vec()),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("a deep generic write under the child is allowed");
        assert_axis_entries_eq!(
            db.indexed_count_top_k([TEST_LEAF, b"cidx"].as_ref(), 5, true, None, gv)
                .unwrap()
                .expect("top_k"),
            vec![(1u64, b"child".to_vec())],
            "the generic path must have carried the new aggregate into the index"
        );
        assert_clean(&db, gv);

        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec(), b"child".to_vec()],
                b"b".to_vec(),
                Element::new_item(b"w".to_vec()),
            )],
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("the same write through the batch path");
        assert_axis_entries_eq!(
            db.indexed_count_top_k([TEST_LEAF, b"cidx"].as_ref(), 5, true, None, gv)
                .unwrap()
                .expect("top_k"),
            vec![(2u64, b"child".to_vec())],
            "the batch path must maintain the index identically"
        );
        assert_clean(&db, gv);
    }
}
