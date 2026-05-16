//! End-to-end tests for `Element::ReferenceWithSumItem` and the
//! `GroveOp::RefreshReferenceWithSumItem` batch op.
//!
//! The variant is a reference that ALSO carries an explicit `SumValue`. It
//! resolves like `Element::Reference` on `get()` (hop-limited, cycle-detected,
//! combined value hash) AND contributes its sum to a sum-bearing parent like
//! `SumItem` / `ItemWithSumItem`. The sum is independent of the resolved
//! target's value.
//!
//! Permitted in any parent tree type — in non-sum parents the carried sum is
//! silently dropped (same rule `ItemWithSumItem` follows).

#[cfg(test)]
mod tests {
    use grovedb_merk::tree::AggregateData;
    use grovedb_version::version::GroveVersion;

    use crate::{
        batch::QualifiedGroveDbOp,
        reference_path::ReferencePathType,
        tests::{make_test_grovedb, TEST_LEAF},
        Element,
    };

    fn insert_target_item(
        db: &crate::GroveDb,
        parent_path: &[&[u8]],
        key: &[u8],
        bytes: &[u8],
        grove_version: &GroveVersion,
    ) {
        db.insert(
            parent_path,
            key,
            Element::new_item(bytes.to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert target item");
    }

    fn open_merk_aggregate(
        db: &crate::GroveDb,
        path: &[&[u8]],
        grove_version: &GroveVersion,
    ) -> AggregateData {
        let transaction = db.start_transaction();
        let merk = db
            .open_transactional_merk_at_path(path.into(), &transaction, None, grove_version)
            .unwrap()
            .expect("open merk");
        merk.aggregate_data().expect("aggregate data")
    }

    /// Insert a `ReferenceWithSumItem` into a `SumTree` parent — its sum
    /// propagates into the parent's running sum.
    #[test]
    fn insert_in_sum_tree_aggregates_sum() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Set up: SumTree under TEST_LEAF/st and a target Item elsewhere.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"st",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert sum tree");

        insert_target_item(
            &db,
            [TEST_LEAF].as_ref(),
            b"target",
            b"target_payload",
            grove_version,
        );

        // Reference-with-sum-item points to the target, carries sum 50.
        let ref_path =
            ReferencePathType::AbsolutePathReference(vec![TEST_LEAF.to_vec(), b"target".to_vec()]);
        let element = Element::new_reference_with_sum_item(ref_path, 50);
        db.insert(
            [TEST_LEAF, b"st"].as_ref(),
            b"link",
            element,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert ref-with-sum-item");

        // The parent SumTree should now total 50.
        let agg = open_merk_aggregate(&db, &[TEST_LEAF, b"st"], grove_version);
        assert_eq!(agg, AggregateData::Sum(50), "sum should propagate");
    }

    /// Two `ReferenceWithSumItem`s in the same SumTree both contribute their
    /// (independent) sums.
    #[test]
    fn multiple_refs_with_sum_items_accumulate() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"st",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert sum tree");

        insert_target_item(&db, [TEST_LEAF].as_ref(), b"target_a", b"a", grove_version);
        insert_target_item(&db, [TEST_LEAF].as_ref(), b"target_b", b"b", grove_version);

        for (key, target, sum) in [
            (b"link_a".as_ref(), b"target_a".as_ref(), 30i64),
            (b"link_b".as_ref(), b"target_b".as_ref(), -8),
            (b"link_c".as_ref(), b"target_a".as_ref(), 100),
        ] {
            let ref_path =
                ReferencePathType::AbsolutePathReference(vec![TEST_LEAF.to_vec(), target.to_vec()]);
            db.insert(
                [TEST_LEAF, b"st"].as_ref(),
                key,
                Element::new_reference_with_sum_item(ref_path, sum),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert link");
        }

        let agg = open_merk_aggregate(&db, &[TEST_LEAF, b"st"], grove_version);
        assert_eq!(agg, AggregateData::Sum(30 + -8 + 100));
    }

    /// In a non-sum parent, the carried sum is silently dropped — same rule
    /// `ItemWithSumItem` follows.
    #[test]
    fn insert_in_normal_tree_sum_silently_ignored() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // TEST_LEAF is a normal tree — the carried sum has nowhere to go.
        insert_target_item(
            &db,
            [TEST_LEAF].as_ref(),
            b"target",
            b"target_payload",
            grove_version,
        );

        let ref_path =
            ReferencePathType::AbsolutePathReference(vec![TEST_LEAF.to_vec(), b"target".to_vec()]);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"link",
            Element::new_reference_with_sum_item(ref_path, 50),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert should succeed in NormalTree");

        // No aggregate data for a normal tree.
        let agg = open_merk_aggregate(&db, &[TEST_LEAF], grove_version);
        assert_eq!(agg, AggregateData::NoAggregateData);

        // The reference is still resolvable to the target's bytes.
        let resolved = db
            .get([TEST_LEAF].as_ref(), b"link", None, grove_version)
            .unwrap()
            .expect("get link");
        assert_eq!(
            resolved,
            Element::new_item(b"target_payload".to_vec()),
            "resolved value is the target item"
        );
    }

    /// `get()` follows the reference to the target item.
    #[test]
    fn get_resolves_to_target_item_bytes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        insert_target_item(
            &db,
            [TEST_LEAF].as_ref(),
            b"target",
            b"payload",
            grove_version,
        );

        let ref_path =
            ReferencePathType::AbsolutePathReference(vec![TEST_LEAF.to_vec(), b"target".to_vec()]);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"link",
            Element::new_reference_with_sum_item(ref_path, 7),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert link");

        let resolved = db
            .get([TEST_LEAF].as_ref(), b"link", None, grove_version)
            .unwrap()
            .expect("get link");
        assert_eq!(resolved, Element::new_item(b"payload".to_vec()));
    }

    /// `get_raw()` returns the new variant unfollowed, preserving the sum.
    #[test]
    fn get_raw_returns_reference_with_sum_item_unfollowed() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        insert_target_item(
            &db,
            [TEST_LEAF].as_ref(),
            b"target",
            b"payload",
            grove_version,
        );

        let ref_path =
            ReferencePathType::AbsolutePathReference(vec![TEST_LEAF.to_vec(), b"target".to_vec()]);
        let element = Element::new_reference_with_sum_item(ref_path.clone(), 21);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"link",
            element.clone(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert link");

        let raw = db
            .get_raw([TEST_LEAF].as_ref().into(), b"link", None, grove_version)
            .unwrap()
            .expect("get_raw link");
        assert_eq!(raw, element, "get_raw must return the variant verbatim");
    }

    /// Two-hop chain: `ReferenceWithSumItem` → `Reference` → `Item`.
    /// `get()` resolves through both reference variants to the terminal
    /// item, exercising the shared chain-follow match arm in
    /// [`crate::operations::get::GroveDb::follow_reference`].
    #[test]
    fn chain_through_reference_with_sum_item_resolves_to_terminal_item() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Terminal item.
        insert_target_item(
            &db,
            [TEST_LEAF].as_ref(),
            b"terminal",
            b"final_payload",
            grove_version,
        );

        // Middle hop: plain Reference → terminal.
        let to_terminal = ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"terminal".to_vec(),
        ]);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"middle",
            Element::new_reference(to_terminal),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert middle");

        // First hop: ReferenceWithSumItem → middle. Both hops are
        // exercised by `get()`.
        let to_middle =
            ReferencePathType::AbsolutePathReference(vec![TEST_LEAF.to_vec(), b"middle".to_vec()]);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"head",
            Element::new_reference_with_sum_item(to_middle, 99),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert head");

        let resolved = db
            .get([TEST_LEAF].as_ref(), b"head", None, grove_version)
            .unwrap()
            .expect("get head");
        assert_eq!(resolved, Element::new_item(b"final_payload".to_vec()));
    }

    /// `NonCounted(ReferenceWithSumItem(_, _, sum, _))` in a `CountSumTree`
    /// parent zeros the count contribution but still propagates the sum.
    /// Mirrors `non_counted_tests.rs` expectations for other base variants.
    #[test]
    fn non_counted_reference_with_sum_item_in_count_sum_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // CountSumTree parent.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cst",
            Element::empty_count_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert count-sum tree");

        insert_target_item(
            &db,
            [TEST_LEAF].as_ref(),
            b"target",
            b"payload",
            grove_version,
        );

        // First insert: bare ReferenceWithSumItem → contributes (1, sum).
        let ref_path =
            ReferencePathType::AbsolutePathReference(vec![TEST_LEAF.to_vec(), b"target".to_vec()]);
        db.insert(
            [TEST_LEAF, b"cst"].as_ref(),
            b"bare_link",
            Element::new_reference_with_sum_item(ref_path.clone(), 25),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert bare link");

        // Second insert: NonCounted wrapper → contributes (0, sum) so count
        // stays at 1 (only `bare_link`) but sum totals 25 + 75 = 100.
        let nc = Element::new_non_counted(Element::new_reference_with_sum_item(ref_path, 75))
            .expect("wrap ok");
        db.insert(
            [TEST_LEAF, b"cst"].as_ref(),
            b"nc_link",
            nc,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert nc link");

        let agg = open_merk_aggregate(&db, &[TEST_LEAF, b"cst"], grove_version);
        assert_eq!(agg, AggregateData::CountAndSum(1, 100));
    }

    /// Inserting a `ReferenceWithSumItem` via a batch (insert-or-replace op)
    /// produces the same parent-sum aggregate as the direct insert path.
    #[test]
    fn batch_insert_reference_with_sum_item_propagates_sum() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"st",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert sum tree");

        insert_target_item(
            &db,
            [TEST_LEAF].as_ref(),
            b"target",
            b"payload",
            grove_version,
        );

        let ref_path =
            ReferencePathType::AbsolutePathReference(vec![TEST_LEAF.to_vec(), b"target".to_vec()]);
        let op = QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            b"link".to_vec(),
            Element::new_reference_with_sum_item(ref_path, 50),
        );
        db.apply_batch(vec![op], None, None, grove_version)
            .unwrap()
            .expect("batch apply");

        let agg = open_merk_aggregate(&db, &[TEST_LEAF, b"st"], grove_version);
        assert_eq!(agg, AggregateData::Sum(50));
    }

    /// `RefreshReferenceWithSumItem` updates the link AND the sum atomically.
    /// The parent SumTree must reflect the delta (new_sum - old_sum).
    #[test]
    fn batch_refresh_reference_with_sum_item_updates_sum_and_path() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"st",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert sum tree");

        insert_target_item(&db, [TEST_LEAF].as_ref(), b"target_a", b"a", grove_version);
        insert_target_item(&db, [TEST_LEAF].as_ref(), b"target_b", b"b", grove_version);

        // Initial insert: link → target_a, sum 10.
        let ref_a = ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"target_a".to_vec(),
        ]);
        db.insert(
            [TEST_LEAF, b"st"].as_ref(),
            b"link",
            Element::new_reference_with_sum_item(ref_a, 10),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");
        assert_eq!(
            open_merk_aggregate(&db, &[TEST_LEAF, b"st"], grove_version),
            AggregateData::Sum(10)
        );

        // Refresh: link → target_b, sum 25. Use trust_refresh_reference = true
        // so we don't need the element on disk to be a `Reference` already.
        let ref_b = ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"target_b".to_vec(),
        ]);
        let refresh = QualifiedGroveDbOp::refresh_reference_with_sum_item_op(
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            b"link".to_vec(),
            ref_b.clone(),
            None,
            25,
            None,
            /* trust_refresh_reference = */ true,
        );
        db.apply_batch(vec![refresh], None, None, grove_version)
            .unwrap()
            .expect("apply refresh");

        // Sum moved by +15.
        assert_eq!(
            open_merk_aggregate(&db, &[TEST_LEAF, b"st"], grove_version),
            AggregateData::Sum(25)
        );

        // The stored variant still resolves to the new target.
        let resolved = db
            .get([TEST_LEAF, b"st"].as_ref(), b"link", None, grove_version)
            .unwrap()
            .expect("get refreshed link");
        assert_eq!(resolved, Element::new_item(b"b".to_vec()));

        // get_raw confirms the new sum is on disk.
        let raw = db
            .get_raw(
                [TEST_LEAF, b"st"].as_ref().into(),
                b"link",
                None,
                grove_version,
            )
            .unwrap()
            .expect("get_raw refreshed link");
        assert_eq!(raw, Element::new_reference_with_sum_item(ref_b, 25));
    }

    /// Applying `RefreshReferenceWithSumItem` against a plain `Reference` on
    /// disk (with `trust_refresh_reference=false`) must be rejected — silent
    /// coercion would corrupt the parent's aggregate.
    #[test]
    fn batch_refresh_cross_type_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"st",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert sum tree");

        insert_target_item(&db, [TEST_LEAF].as_ref(), b"target", b"x", grove_version);

        // Insert a *plain* Reference (no sum) on disk.
        let ref_path =
            ReferencePathType::AbsolutePathReference(vec![TEST_LEAF.to_vec(), b"target".to_vec()]);
        db.insert(
            [TEST_LEAF, b"st"].as_ref(),
            b"link",
            Element::new_reference(ref_path.clone()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");

        // Refresh as RefreshReferenceWithSumItem (no trust). The apply path
        // must reject because the on-disk variant is `Reference`, not
        // `ReferenceWithSumItem`.
        let refresh = QualifiedGroveDbOp::refresh_reference_with_sum_item_op(
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            b"link".to_vec(),
            ref_path,
            None,
            42,
            None,
            /* trust_refresh_reference = */ false,
        );
        let err = db
            .apply_batch(vec![refresh], None, None, grove_version)
            .unwrap()
            .expect_err("cross-type refresh must fail");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("RefWithSumItem")
                || msg.contains("ReferenceWithSumItem")
                || msg.contains("non-RefWithSumItem"),
            "expected cross-type rejection error, got: {msg}"
        );
    }

    /// `is_reference` and `is_reference_with_sum_item` predicates work in
    /// the end-to-end pipeline (post-deserialization).
    #[test]
    fn predicates_persist_through_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        insert_target_item(
            &db,
            [TEST_LEAF].as_ref(),
            b"target",
            b"payload",
            grove_version,
        );

        let ref_path =
            ReferencePathType::AbsolutePathReference(vec![TEST_LEAF.to_vec(), b"target".to_vec()]);
        let element = Element::new_reference_with_sum_item(ref_path, 11);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"link",
            element,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");

        let raw = db
            .get_raw([TEST_LEAF].as_ref().into(), b"link", None, grove_version)
            .unwrap()
            .expect("get_raw");
        assert!(raw.is_reference());
        assert!(raw.is_reference_with_sum_item());
        assert!(!raw.is_any_item());
        assert_eq!(raw.sum_value_or_default(), 11);
    }

    /// Inserting a `NotSummed(ReferenceWithSumItem(..))` is rejected at
    /// construction — `NotSummed` only wraps sum-tree variants, not
    /// reference-like leaves.
    #[test]
    fn new_not_summed_rejects_reference_with_sum_item() {
        let ref_path = ReferencePathType::AbsolutePathReference(vec![b"a".to_vec()]);
        let inner = Element::new_reference_with_sum_item(ref_path, 1);
        assert!(Element::new_not_summed(inner).is_err());
    }
}
