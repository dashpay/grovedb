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
    use grovedb_merk::{
        proofs::{query::QueryItem, Query},
        tree::AggregateData,
    };
    use grovedb_version::version::GroveVersion;

    use crate::{
        batch::QualifiedGroveDbOp,
        operations::get::QueryItemOrSumReturnType,
        reference_path::ReferencePathType,
        tests::{make_test_grovedb, TEST_LEAF},
        Element, GroveDb, PathQuery,
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
            /* non_counted = */ false,
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
            /* non_counted = */ false,
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

    /// Cmp / Ord routes through `GroveOp::to_u8`. The new op's wire tag
    /// must be stable at 17 (next free after `InsertNonMerkTree = 16`),
    /// since that byte is part of the batch-serialization wire format.
    #[test]
    fn refresh_reference_with_sum_item_op_tag_pin() {
        use std::cmp::Ordering;

        let ref_path = ReferencePathType::AbsolutePathReference(vec![b"a".to_vec()]);
        let refresh = QualifiedGroveDbOp::refresh_reference_with_sum_item_op(
            vec![b"p".to_vec()],
            b"k".to_vec(),
            ref_path,
            None,
            5,
            None,
            false,
            true,
        )
        .op;
        let delete = QualifiedGroveDbOp::delete_op(vec![b"p".to_vec()], b"k".to_vec()).op;
        let insert = QualifiedGroveDbOp::insert_or_replace_op(
            vec![b"p".to_vec()],
            b"k".to_vec(),
            Element::new_item(b"x".to_vec()),
        )
        .op;

        // delete (to_u8 = 2) sorts before refresh-ref-with-sum-item (= 17)
        // which sorts after every other user-facing op.
        assert_eq!(delete.cmp(&refresh), Ordering::Less);
        assert_eq!(insert.cmp(&refresh), Ordering::Less);
        assert_eq!(refresh.cmp(&refresh.clone()), Ordering::Equal);
    }

    /// Debug formatter for `GroveOp::RefreshReferenceWithSumItem`
    /// produces a string containing the path, max_hop, sum, and trust
    /// flag — exercises the `fmt::Debug` arm.
    #[test]
    fn refresh_reference_with_sum_item_debug_format() {
        let op = QualifiedGroveDbOp::refresh_reference_with_sum_item_op(
            vec![b"parent".to_vec()],
            b"child".to_vec(),
            ReferencePathType::AbsolutePathReference(vec![b"target".to_vec()]),
            Some(3),
            42,
            None,
            false,
            true,
        );
        let s = format!("{op:?}");
        assert!(
            s.contains("Refresh Reference With Sum Item"),
            "Debug should include op name: {s}"
        );
        assert!(s.contains("max_hop"), "Debug should mention max_hop: {s}");
        assert!(s.contains("sum 42"), "Debug should include the sum: {s}");
        assert!(
            s.contains("trust_reference true"),
            "Debug should include trust flag: {s}"
        );
    }

    /// Trusted refresh with `non_counted = true` against a CountSumTree
    /// parent goes through `Element::new_non_counted` on the rebuilt
    /// inner. Reaffirms the wrap-on-write block in the trust=true path.
    /// CountSumTree is both count- and sum-bearing, which is the only
    /// parent that accepts NonCounted-wrapped reference variants.
    #[test]
    fn batch_refresh_reference_with_sum_item_trusted_with_nc_wraps() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

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
        insert_target_item(&db, [TEST_LEAF].as_ref(), b"target", b"x", grove_version);

        // Seed link with NonCounted(RefWithSum(_, _, 8, _)).
        let ref_path =
            ReferencePathType::AbsolutePathReference(vec![TEST_LEAF.to_vec(), b"target".to_vec()]);
        let nc =
            Element::new_non_counted(Element::new_reference_with_sum_item(ref_path.clone(), 8))
                .expect("wrap ok");
        db.insert(
            [TEST_LEAF, b"cst"].as_ref(),
            b"link",
            nc,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("seed nc link");
        assert_eq!(
            open_merk_aggregate(&db, &[TEST_LEAF, b"cst"], grove_version),
            AggregateData::CountAndSum(0, 8),
        );

        // Trusted refresh with non_counted=true rewraps in NonCounted.
        let refresh = QualifiedGroveDbOp::refresh_reference_with_sum_item_op(
            vec![TEST_LEAF.to_vec(), b"cst".to_vec()],
            b"link".to_vec(),
            ref_path,
            None,
            33,
            None,
            /* non_counted = */ true,
            /* trust_refresh_reference = */ true,
        );
        db.apply_batch(vec![refresh], None, None, grove_version)
            .unwrap()
            .expect("apply trusted refresh");

        assert_eq!(
            open_merk_aggregate(&db, &[TEST_LEAF, b"cst"], grove_version),
            AggregateData::CountAndSum(0, 33),
        );
        // Wrapper preserved on disk.
        let raw = db
            .get_raw(
                [TEST_LEAF, b"cst"].as_ref().into(),
                b"link",
                None,
                grove_version,
            )
            .unwrap()
            .expect("get_raw");
        assert!(raw.is_non_counted());
    }

    /// Untrusted refresh against a NonCounted(RefWithSum) with
    /// `non_counted = true` succeeds — the disk shape matches the
    /// declaration and the wrapper is preserved.
    #[test]
    fn batch_refresh_reference_with_sum_item_untrusted_matches_wrapper_succeeds() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

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
        insert_target_item(&db, [TEST_LEAF].as_ref(), b"target", b"x", grove_version);

        // Seed wrapped variant.
        let ref_path =
            ReferencePathType::AbsolutePathReference(vec![TEST_LEAF.to_vec(), b"target".to_vec()]);
        let nc =
            Element::new_non_counted(Element::new_reference_with_sum_item(ref_path.clone(), 1))
                .expect("wrap ok");
        db.insert(
            [TEST_LEAF, b"cst"].as_ref(),
            b"link",
            nc,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("seed");

        // Untrusted refresh with matching non_counted=true.
        let refresh = QualifiedGroveDbOp::refresh_reference_with_sum_item_op(
            vec![TEST_LEAF.to_vec(), b"cst".to_vec()],
            b"link".to_vec(),
            ref_path,
            None,
            7,
            None,
            /* non_counted = */ true,
            /* trust_refresh_reference = */ false,
        );
        db.apply_batch(vec![refresh], None, None, grove_version)
            .unwrap()
            .expect("apply untrusted refresh");

        assert_eq!(
            open_merk_aggregate(&db, &[TEST_LEAF, b"cst"], grove_version),
            AggregateData::CountAndSum(0, 7),
        );
        let raw = db
            .get_raw(
                [TEST_LEAF, b"cst"].as_ref().into(),
                b"link",
                None,
                grove_version,
            )
            .unwrap()
            .expect("get_raw");
        assert!(raw.is_non_counted());
    }

    /// Regression test for the wrapper-invariant bypass: a trusted
    /// `RefreshReferenceWithSumItem` with `non_counted = true` in a
    /// non-count-bearing parent (here, a `NormalTree` under TEST_LEAF)
    /// must be rejected. Without the apply-path guard, the trusted
    /// branch would build `NonCounted(...)` and persist it into the
    /// wrong tree type — violating the invariant that NonCounted-wrapped
    /// elements only live in count-bearing trees.
    #[test]
    fn batch_refresh_reference_with_sum_item_trusted_with_nc_rejected_in_normal_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        insert_target_item(&db, [TEST_LEAF].as_ref(), b"target", b"x", grove_version);

        // Seed bare ReferenceWithSumItem in a NormalTree (TEST_LEAF).
        let ref_path =
            ReferencePathType::AbsolutePathReference(vec![TEST_LEAF.to_vec(), b"target".to_vec()]);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"link",
            Element::new_reference_with_sum_item(ref_path.clone(), 1),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("seed bare ref-with-sum-item");

        // Trusted refresh with non_counted=true. Without the guard the
        // apply path would build NonCounted(...) and write it to disk
        // under TEST_LEAF (NormalTree), silently violating the invariant.
        let refresh = QualifiedGroveDbOp::refresh_reference_with_sum_item_op(
            vec![TEST_LEAF.to_vec()],
            b"link".to_vec(),
            ref_path,
            None,
            5,
            None,
            /* non_counted = */ true,
            /* trust_refresh_reference = */ true,
        );
        let err = db
            .apply_batch(vec![refresh], None, None, grove_version)
            .unwrap()
            .expect_err("trusted refresh with non_counted=true in normal tree must fail");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("count-bearing") || msg.contains("non_counted"),
            "expected wrapper-invariant rejection, got: {msg}"
        );

        // And the on-disk shape is unchanged — still bare, count is 1.
        let raw = db
            .get_raw([TEST_LEAF].as_ref().into(), b"link", None, grove_version)
            .unwrap()
            .expect("get_raw");
        assert!(!raw.is_non_counted(), "wrapper must not have been written");
    }

    /// Regression test for the [P1] finding: a dependent reference
    /// re-inserted in the same batch as a `RefreshReferenceWithSumItem`
    /// of its target must commit against the **refreshed** target's
    /// value hash, not the stale on-disk one.
    ///
    /// Pre-fix: `process_reference` had a `recursions_allowed == 1`
    /// fast path that called `merk.get_value_hash(target_key)` —
    /// returning the on-disk hash even when the target was being
    /// refreshed in the same batch, AND returning the wrong hash for
    /// Reference targets (combined merk hash, not the terminal's simple
    /// hash). `verify_grovedb` would report a mismatch.
    ///
    /// Post-fix: the fast path is removed; in-batch refresh targets are
    /// always resolved through the op's new path, and on-disk targets
    /// are read and dispatched by type. `verify_grovedb` stays clean.
    #[test]
    fn batch_dependent_ref_resolves_through_refreshed_path_via_chain() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        insert_target_item(
            &db,
            [TEST_LEAF].as_ref(),
            b"target_a",
            b"AAAA",
            grove_version,
        );
        insert_target_item(
            &db,
            [TEST_LEAF].as_ref(),
            b"target_b",
            b"BBBB",
            grove_version,
        );

        // `link` is a ReferenceWithSumItem currently pointing at target_a.
        let to_a = ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"target_a".to_vec(),
        ]);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"link",
            Element::new_reference_with_sum_item(to_a, 1),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("seed link");

        // `dep` is a plain Reference → link. No max_hop set (defaults
        // to MAX_REFERENCE_HOPS) so the chain dep → link → target can
        // resolve all the way to the terminal Item, which is the budget
        // the direct insert path and `verify_grovedb` use.
        let to_link =
            ReferencePathType::AbsolutePathReference(vec![TEST_LEAF.to_vec(), b"link".to_vec()]);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"dep",
            Element::new_reference(to_link.clone()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("seed dep");

        // Pre-batch: verify_grovedb should be clean.
        let issues_before = db
            .verify_grovedb(None, true, true, grove_version)
            .expect("verify pre-batch");
        assert!(
            issues_before.is_empty(),
            "pre-batch verify should be clean, got: {issues_before:?}"
        );

        // Batch: refresh `link` to point at target_b AND re-insert `dep`
        // so its merk-stored value_hash gets recomputed. After the fix,
        // dep's stored hash must combine with target_b's simple hash
        // (the chain's terminal), not link's old merk-combined hash.
        let to_b = ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"target_b".to_vec(),
        ]);
        let refresh = QualifiedGroveDbOp::refresh_reference_with_sum_item_op(
            vec![TEST_LEAF.to_vec()],
            b"link".to_vec(),
            to_b,
            None,
            2,
            None,
            /* non_counted = */ false,
            /* trust_refresh_reference = */ true,
        );
        let dep_replace = QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"dep".to_vec(),
            Element::new_reference(to_link),
        );
        db.apply_batch(vec![refresh, dep_replace], None, None, grove_version)
            .unwrap()
            .expect("apply batch");

        // User-facing get follows the chain at read time.
        let resolved = db
            .get([TEST_LEAF].as_ref(), b"dep", None, grove_version)
            .unwrap()
            .expect("get dep");
        assert_eq!(
            resolved,
            Element::new_item(b"BBBB".to_vec()),
            "dep should resolve to target_b after refresh"
        );

        // Load-bearing check: verify_grovedb must NOT report any hash
        // mismatches. Pre-fix this failed with a mismatch on [test_leaf,
        // dep] because dep was committed against the stale link hash.
        let issues_after = db
            .verify_grovedb(None, true, true, grove_version)
            .expect("verify post-batch");
        assert!(
            issues_after.is_empty(),
            "post-batch verify must be clean after the P1 fix; got: {issues_after:?}"
        );
    }

    /// Companion test for the [P1] fix: a 1-hop reference (`max_hop =
    /// Some(1)`) that points at another reference is rejected at batch
    /// time with `ReferenceLimit`, because the chain depth (2+) exceeds
    /// the user-declared budget. Documents the strict `max_hop`
    /// enforcement that the test suite relies on (see
    /// `batch::tests::test_references` for the canonical example).
    /// Pre-fix this case silently committed a stale/wrong hash; the
    /// fix replaces the silent corruption with an explicit error.
    #[test]
    fn batch_one_hop_dependent_ref_into_ref_chain_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        insert_target_item(&db, [TEST_LEAF].as_ref(), b"target", b"x", grove_version);
        let to_target =
            ReferencePathType::AbsolutePathReference(vec![TEST_LEAF.to_vec(), b"target".to_vec()]);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"link",
            Element::new_reference_with_sum_item(to_target, 1),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("seed link");

        // dep with max_hop=Some(1) → link (which is itself a reference).
        // Batch insert must reject because the chain depth exceeds 1.
        let to_link =
            ReferencePathType::AbsolutePathReference(vec![TEST_LEAF.to_vec(), b"link".to_vec()]);
        let dep_insert = QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"dep".to_vec(),
            Element::new_reference_with_hops(to_link, Some(1)),
        );
        let err = db
            .apply_batch(vec![dep_insert], None, None, grove_version)
            .unwrap()
            .expect_err("batch insert of 1-hop ref-into-ref must fail");
        assert!(
            matches!(err, crate::Error::ReferenceLimit),
            "expected ReferenceLimit, got: {err:?}"
        );
    }

    /// `RefreshReferenceWithSumItem` against a non-existing key with
    /// `trust=false` errors out — exercises the "trying to refresh a
    /// non existing reference" branch in the apply path.
    #[test]
    fn batch_refresh_reference_with_sum_item_untrusted_missing_key_errors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let ref_path = ReferencePathType::AbsolutePathReference(vec![b"x".to_vec()]);
        let refresh = QualifiedGroveDbOp::refresh_reference_with_sum_item_op(
            vec![TEST_LEAF.to_vec()],
            b"does_not_exist".to_vec(),
            ref_path,
            None,
            1,
            None,
            /* non_counted = */ false,
            /* trust_refresh_reference = */ false,
        );
        let err = db
            .apply_batch(vec![refresh], None, None, grove_version)
            .unwrap()
            .expect_err("refresh of non-existing key must fail");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("non existing") || msg.contains("not") || msg.contains("MissingReference"),
            "expected missing-key error, got: {msg}"
        );
    }

    /// `query_item_value` follows a `ReferenceWithSumItem` to the target
    /// item bytes — same as `Reference`. Exercises the new arm in
    /// [`crate::operations::get::query::GroveDb::query_item_value`].
    #[test]
    fn query_item_value_follows_reference_with_sum_item() {
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
            Element::new_reference_with_sum_item(ref_path, 99),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert link");

        let mut query = Query::new();
        query.insert_key(b"link".to_vec());
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        let (items, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("query_item_value should succeed");
        assert_eq!(items, vec![b"payload".to_vec()]);
    }

    /// `query_item_value_or_sum` follows a `ReferenceWithSumItem` and
    /// returns the target item (same shape as following a `Reference`).
    #[test]
    fn query_item_value_or_sum_follows_reference_with_sum_item() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // SumItem must live inside a sum-bearing tree.
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
        db.insert(
            [TEST_LEAF, b"st"].as_ref(),
            b"target",
            Element::new_sum_item(50),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert sum item target");

        let ref_path = ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"st".to_vec(),
            b"target".to_vec(),
        ]);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"link",
            // Carried sum (999) is independent of target's sum (50); the
            // query returns the target's sum.
            Element::new_reference_with_sum_item(ref_path, 999),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert link");

        let mut query = Query::new();
        query.insert_key(b"link".to_vec());
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        let (items_or_sums, _) = db
            .query_item_value_or_sum(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("query_item_value_or_sum should succeed");
        assert_eq!(items_or_sums, vec![QueryItemOrSumReturnType::SumValue(50)]);
    }

    /// `query_sums` follows a `ReferenceWithSumItem` chain to a `SumItem`
    /// target and returns the **target's** sum, not the carried sum.
    #[test]
    fn query_sums_follows_reference_with_sum_item_to_sum_item() {
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
        db.insert(
            [TEST_LEAF, b"st"].as_ref(),
            b"target",
            Element::new_sum_item(77),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert sum item");

        let ref_path = ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"st".to_vec(),
            b"target".to_vec(),
        ]);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"link",
            Element::new_reference_with_sum_item(ref_path, 999),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert link");

        let mut query = Query::new();
        query.insert_key(b"link".to_vec());
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        let (sums, _) = db
            .query_sums(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("query_sums should succeed");
        assert_eq!(sums, vec![77]);
    }

    /// `query_encoded_many` (multi-path) resolves a `ReferenceWithSumItem`
    /// to its terminal item — covers the multi-path query arm in `query.rs`
    /// near line 98.
    #[test]
    #[allow(deprecated)]
    fn query_encoded_many_follows_reference_with_sum_item() {
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

        let mut query = Query::new();
        query.insert_key(b"link".to_vec());
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        let items = db
            .query_encoded_many(&[&path_query], true, true, true, None, grove_version)
            .unwrap()
            .expect("query_encoded_many should succeed");
        assert_eq!(items, vec![b"payload".to_vec()]);
    }

    /// `RefreshReferenceWithSumItem` with `trust_refresh_reference = false`
    /// reads the on-disk element to verify it is also a
    /// `ReferenceWithSumItem` before applying the update. This exercises
    /// the disk-read branch in the batch apply path.
    #[test]
    fn batch_refresh_reference_with_sum_item_untrusted() {
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
        .expect("seed link");

        // Refresh with trust=false → batch path reads the on-disk element,
        // confirms it is RefWithSumItem, then rebuilds with the new path
        // and sum.
        let ref_b = ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"target_b".to_vec(),
        ]);
        let refresh = QualifiedGroveDbOp::refresh_reference_with_sum_item_op(
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            b"link".to_vec(),
            ref_b.clone(),
            None,
            42,
            None,
            /* non_counted = */ false,
            /* trust_refresh_reference = */ false,
        );
        db.apply_batch(vec![refresh], None, None, grove_version)
            .unwrap()
            .expect("untrusted refresh ref-with-sum-item should succeed");

        // Confirm new sum is on disk.
        let raw = db
            .get_raw(
                [TEST_LEAF, b"st"].as_ref().into(),
                b"link",
                None,
                grove_version,
            )
            .unwrap()
            .expect("get_raw refreshed link");
        assert_eq!(raw, Element::new_reference_with_sum_item(ref_b, 42));
    }

    /// `RefreshReferenceWithSumItem` with `non_counted=true` and
    /// `trust_refresh_reference=true` rebuilds the element wrapped in
    /// `NonCounted`. This locks in the wrapper-preservation behavior the
    /// `non_counted` field on the op exists to provide.
    #[test]
    fn batch_refresh_reference_with_sum_item_trusted_preserves_non_counted_wrapper() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // CountSumTree parent so count + sum aggregates are observable.
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
        insert_target_item(&db, [TEST_LEAF].as_ref(), b"target", b"x", grove_version);

        // Seed a NonCounted(ReferenceWithSumItem) with sum 10. Count
        // contribution is 0 (NonCounted), sum contribution is 10.
        let ref_path =
            ReferencePathType::AbsolutePathReference(vec![TEST_LEAF.to_vec(), b"target".to_vec()]);
        let nc_initial =
            Element::new_non_counted(Element::new_reference_with_sum_item(ref_path.clone(), 10))
                .expect("wrap ok");
        db.insert(
            [TEST_LEAF, b"cst"].as_ref(),
            b"link",
            nc_initial,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("seed nc link");
        assert_eq!(
            open_merk_aggregate(&db, &[TEST_LEAF, b"cst"], grove_version),
            AggregateData::CountAndSum(0, 10),
        );

        // Trusted refresh with non_counted=true must preserve the wrapper.
        let refresh = QualifiedGroveDbOp::refresh_reference_with_sum_item_op(
            vec![TEST_LEAF.to_vec(), b"cst".to_vec()],
            b"link".to_vec(),
            ref_path.clone(),
            None,
            25,
            None,
            /* non_counted = */ true,
            /* trust_refresh_reference = */ true,
        );
        db.apply_batch(vec![refresh], None, None, grove_version)
            .unwrap()
            .expect("apply trusted refresh");

        // Count stays 0 (still NonCounted), sum becomes 25.
        assert_eq!(
            open_merk_aggregate(&db, &[TEST_LEAF, b"cst"], grove_version),
            AggregateData::CountAndSum(0, 25),
        );
        // On-disk shape is still NonCounted(ReferenceWithSumItem(_, _, 25, _)).
        let raw = db
            .get_raw(
                [TEST_LEAF, b"cst"].as_ref().into(),
                b"link",
                None,
                grove_version,
            )
            .unwrap()
            .expect("get_raw");
        assert!(raw.is_non_counted(), "wrapper preserved after refresh");
        assert_eq!(raw.sum_value_or_default(), 25);
    }

    /// `RefreshReferenceWithSumItem` with `trust_refresh_reference=false`
    /// and `non_counted` flag disagreeing with disk is rejected — silent
    /// wrapper drop or injection would corrupt the parent's count
    /// aggregate.
    #[test]
    fn batch_refresh_reference_with_sum_item_untrusted_rejects_wrapper_mismatch() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        insert_target_item(&db, [TEST_LEAF].as_ref(), b"target", b"x", grove_version);

        // Seed a BARE ReferenceWithSumItem (not wrapped) under TEST_LEAF.
        let ref_path =
            ReferencePathType::AbsolutePathReference(vec![TEST_LEAF.to_vec(), b"target".to_vec()]);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"link",
            Element::new_reference_with_sum_item(ref_path.clone(), 1),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("seed bare ref-with-sum-item");

        // Untrusted refresh with non_counted=true must reject because the
        // on-disk element is bare.
        let bad = QualifiedGroveDbOp::refresh_reference_with_sum_item_op(
            vec![TEST_LEAF.to_vec()],
            b"link".to_vec(),
            ref_path,
            None,
            7,
            None,
            /* non_counted = */ true,
            /* trust_refresh_reference = */ false,
        );
        let err = db
            .apply_batch(vec![bad], None, None, grove_version)
            .unwrap()
            .expect_err("wrapper-mismatch refresh must fail");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("non_counted") || msg.contains("wrapper") || msg.contains("disagrees"),
            "expected wrapper-mismatch rejection, got: {msg}"
        );
    }

    /// Regression test for the "stale dependent reference" issue: when a
    /// batch contains both a `RefreshReferenceWithSumItem` op AND another
    /// reference that points at the same key, the dependent reference's
    /// value hash must be computed against the **refreshed** target, not
    /// the stale on-disk one. Verified for both `trust=true` and
    /// `trust=false` paths.
    #[test]
    fn batch_dependent_reference_resolves_through_refreshed_path() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Two distinct items so we can prove the dependent ref tracks the
        // post-batch path.
        insert_target_item(
            &db,
            [TEST_LEAF].as_ref(),
            b"item_old",
            b"OLD",
            grove_version,
        );
        insert_target_item(
            &db,
            [TEST_LEAF].as_ref(),
            b"item_new",
            b"NEW",
            grove_version,
        );

        // Seed: `link` is a ReferenceWithSumItem → item_old (sum 1).
        // `dep` is a plain Reference → link → item_old.
        let to_old = ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"item_old".to_vec(),
        ]);
        let to_link =
            ReferencePathType::AbsolutePathReference(vec![TEST_LEAF.to_vec(), b"link".to_vec()]);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"link",
            Element::new_reference_with_sum_item(to_old.clone(), 1),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("seed link");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"dep",
            Element::new_reference(to_link.clone()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("seed dep");

        // Batch: refresh link → item_new (trust=false so we hit the
        // resolve-through-op-payload branch via process_reference), AND
        // re-insert dep so its value hash gets re-derived in the same
        // batch. dep's hash must derive from item_new (NEW), not item_old.
        let to_new = ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"item_new".to_vec(),
        ]);
        let refresh = QualifiedGroveDbOp::refresh_reference_with_sum_item_op(
            vec![TEST_LEAF.to_vec()],
            b"link".to_vec(),
            to_new.clone(),
            None,
            99,
            None,
            /* non_counted = */ false,
            /* trust_refresh_reference = */ false,
        );
        let dep_replace = QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"dep".to_vec(),
            Element::new_reference(to_link),
        );
        db.apply_batch(vec![refresh, dep_replace], None, None, grove_version)
            .unwrap()
            .expect("apply refresh+dep batch");

        // After the batch, getting dep must follow link (now pointing at
        // item_new) and return "NEW", not "OLD". This is the user-visible
        // proof that the batch's internal hash computation used the
        // refreshed path.
        let resolved = db
            .get([TEST_LEAF].as_ref(), b"dep", None, grove_version)
            .unwrap()
            .expect("get dep");
        assert_eq!(
            resolved,
            Element::new_item(b"NEW".to_vec()),
            "dependent ref should resolve through the refreshed path"
        );
    }

    /// `prove_query` + `verify_query_with_options` round-trip on a
    /// `ReferenceWithSumItem` — exercises the V1 proof generation /
    /// verification arms for the new variant.
    #[test]
    fn prove_and_verify_reference_with_sum_item() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

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
            Element::new_reference_with_sum_item(ref_path, 7),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert ref-with-sum-item");

        let path_query = PathQuery::new_unsized(
            vec![TEST_LEAF.to_vec()],
            Query {
                items: vec![QueryItem::Key(b"link".to_vec())],
                default_subquery_branch: Default::default(),
                left_to_right: true,
                conditional_subquery_branches: None,
                add_parent_tree_on_subquery: false,
            },
        );

        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove ref-with-sum-item");

        let (root_hash, result_set) = GroveDb::verify_query_with_options(
            &proof,
            &path_query,
            grovedb_merk::proofs::query::VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("verify ref-with-sum-item proof");

        let expected_root = db.grove_db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root, "root hash should match");
        assert_eq!(result_set.len(), 1, "proof should return 1 result");
        // The resolved value is the target item's payload (reference was
        // followed in the proof post-processing step).
        let (_path, key, element) = &result_set[0];
        assert_eq!(key, b"link");
        let element = element.as_ref().expect("element should be Some");
        match element {
            Element::Item(bytes, _) => assert_eq!(bytes, b"target_payload"),
            other => panic!("expected resolved target Item, got {:?}", other),
        }
    }
}
