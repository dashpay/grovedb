//! Coverage tests targeting uncovered diff lines of PR #657.

#[cfg(test)]
mod tests {
    use grovedb_costs::OperationCost;
    use grovedb_element::indexed::IndexAxis;
    use grovedb_merk::{
        estimated_costs::average_case_costs::{
            EstimatedLayerCount, EstimatedLayerInformation, EstimatedLayerSizes,
        },
        proofs::Query as MerkQuery,
        tree_type::TreeType,
    };
    use grovedb_version::version::GroveVersion;

    use crate::{
        batch::KeyInfoPath,
        operations::get::QueryItemOrSumReturnType,
        reference_path::ReferencePathType,
        tests::{make_test_grovedb, TEST_LEAF},
        Element, GroveDb, PathQuery, SizedQuery,
    };

    // =================================================================
    // operations/get/query.rs — `query_item_value_or_sum` dispatch for
    // the three indexed-tree element types, both when the queried
    // element IS the indexed tree and when a reference resolves to one.
    // =================================================================

    /// Builds `[TEST_LEAF]/a_pcit` (PCIT, child counts 1 + 5),
    /// `[TEST_LEAF]/b_psit` (PSIT, sums 7 + -3) and
    /// `[TEST_LEAF]/c_pcpsit` (PCPSIT, two `ItemWithSumItem` of 5 and 10).
    ///
    /// The PCIT child counts are DERIVED: each child subtree is inserted
    /// EMPTY and then populated with that many items, so propagation
    /// supplies the aggregate. The dedicated indexed insert rejects a
    /// rootless child that merely claims a non-zero count — there would
    /// be nothing to derive it from, and the claim would become the
    /// authenticated secondary sort key.
    fn build_indexed_trees(db: &GroveDb, grove_version: &GroveVersion) {
        db.insert(
            [TEST_LEAF].as_ref(),
            b"a_pcit",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PCIT");
        for (k, c) in [(b"x" as &[u8], 1u64), (b"y" as &[u8], 5)] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"a_pcit"].as_ref(),
                k,
                Element::empty_provable_count_tree(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert empty PCIT child");
            for i in 0..c {
                db.insert(
                    [TEST_LEAF, b"a_pcit", k].as_ref(),
                    &i.to_be_bytes(),
                    Element::new_item(b"v".to_vec()),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("populate PCIT child");
            }
        }

        db.insert(
            [TEST_LEAF].as_ref(),
            b"b_psit",
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PSIT");
        for (k, s) in [(b"x" as &[u8], 7i64), (b"y" as &[u8], -3)] {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"b_psit"].as_ref(),
                k,
                Element::new_sum_item(s),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert PSIT entry");
        }

        db.insert(
            [TEST_LEAF].as_ref(),
            b"c_pcpsit",
            Element::empty_provable_count_provable_sum_indexed_tree(vec![
                (IndexAxis::Count.tag(), None),
                (IndexAxis::Sum.tag(), None),
            ])
            .expect("canonical axes"),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PCPSIT");
        for (k, s) in [(b"x" as &[u8], 5i64), (b"y" as &[u8], 10)] {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"c_pcpsit"].as_ref(),
                k,
                Element::new_item_with_sum_item(b"v".to_vec(), s),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert PCPSIT entry");
        }
    }

    /// The aggregates the three trees carry after `build_indexed_trees`,
    /// read back straight off the stored elements so the query
    /// expectations below are pinned to the same source of truth.
    fn stored_aggregates(db: &GroveDb, grove_version: &GroveVersion) -> (u64, i64, (u64, i64)) {
        let pcit = db
            .get_raw([TEST_LEAF].as_ref().into(), b"a_pcit", None, grove_version)
            .unwrap()
            .expect("fetch PCIT");
        let count = match pcit {
            Element::ProvableCountIndexedTree(_, _, count, _) => count,
            other => panic!("expected PCIT, got {other}"),
        };
        let psit = db
            .get_raw([TEST_LEAF].as_ref().into(), b"b_psit", None, grove_version)
            .unwrap()
            .expect("fetch PSIT");
        let sum = match psit {
            Element::ProvableSumIndexedTree(_, _, sum, _) => sum,
            other => panic!("expected PSIT, got {other}"),
        };
        let pcpsit = db
            .get_raw(
                [TEST_LEAF].as_ref().into(),
                b"c_pcpsit",
                None,
                grove_version,
            )
            .unwrap()
            .expect("fetch PCPSIT");
        let count_sum = match pcpsit {
            Element::ProvableCountProvableSumIndexedTree(_, count, sum, _, _) => (count, sum),
            other => panic!("expected PCPSIT, got {other}"),
        };
        (count, sum, count_sum)
    }

    /// A query whose results ARE the indexed-tree elements maps each of
    /// them to its aggregate return type: PCIT -> `CountValue`,
    /// PSIT -> `SumValue`, PCPSIT -> `CountSumValue`.
    #[test]
    fn query_item_value_or_sum_maps_indexed_tree_elements_to_aggregates() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_indexed_trees(&db, grove_version);

        let (count, sum, (pcpsit_count, pcpsit_sum)) = stored_aggregates(&db, grove_version);
        // Two entries with counts 1 and 5 aggregate to 6; sums 7 and -3
        // aggregate to 4; the PCPSIT counts its two entries and sums
        // 5 + 10.
        assert_eq!(count, 6);
        assert_eq!(sum, 4);
        assert_eq!((pcpsit_count, pcpsit_sum), (2, 15));

        let mut query = MerkQuery::new();
        query.insert_all();
        let path_query =
            PathQuery::new(vec![TEST_LEAF.to_vec()], SizedQuery::new(query, None, None));

        let (results, skipped) = db
            .query_item_value_or_sum(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("query_item_value_or_sum over indexed trees");

        assert_eq!(skipped, 0);
        assert_eq!(
            results,
            vec![
                QueryItemOrSumReturnType::CountValue(6),
                QueryItemOrSumReturnType::SumValue(4),
                QueryItemOrSumReturnType::CountSumValue(2, 15),
            ]
        );
    }

    /// Same three element types, but reached through absolute-path
    /// references: `follow_reference` resolves to the indexed tree and
    /// the reference branch maps the aggregates identically.
    #[test]
    fn query_item_value_or_sum_follows_references_to_indexed_trees() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_indexed_trees(&db, grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"refs",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create refs tree");
        for (ref_key, target) in [
            (b"r1" as &[u8], b"a_pcit" as &[u8]),
            (b"r2", b"b_psit"),
            (b"r3", b"c_pcpsit"),
        ] {
            db.insert(
                [TEST_LEAF, b"refs"].as_ref(),
                ref_key,
                Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                    TEST_LEAF.to_vec(),
                    target.to_vec(),
                ])),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert reference to indexed tree");
        }

        let mut query = MerkQuery::new();
        query.insert_all();
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"refs".to_vec()],
            SizedQuery::new(query, None, None),
        );

        let (results, skipped) = db
            .query_item_value_or_sum(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("query_item_value_or_sum over references to indexed trees");

        assert_eq!(skipped, 0);
        assert_eq!(
            results,
            vec![
                QueryItemOrSumReturnType::CountValue(6),
                QueryItemOrSumReturnType::SumValue(4),
                QueryItemOrSumReturnType::CountSumValue(2, 15),
            ]
        );
    }

    // =================================================================
    // estimated_costs/average_case_costs.rs —
    // `average_case_indexed_secondary_mirror` and the private
    // `average_case_layer_key_size` it derives the secondary key width
    // from.
    // =================================================================

    fn mirror_cost(sizes: EstimatedLayerSizes, axes: &[IndexAxis]) -> OperationCost {
        let grove_version = GroveVersion::latest();
        let primary_path = KeyInfoPath::from_known_path([b"primary".as_ref()]);
        let layer = EstimatedLayerInformation {
            tree_type: TreeType::ProvableCountProvableSumIndexedTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(100),
            estimated_layer_sizes: sizes,
        };
        let result = GroveDb::average_case_indexed_secondary_mirror(
            &primary_path,
            &layer,
            axes,
            grove_version,
        );
        result
            .value
            .as_ref()
            .expect("indexed secondary mirror estimate should succeed");
        result.cost
    }

    /// Every non-`Mix` layer-size variant describes its average key size
    /// in the same leading field, so the derived secondary cost is
    /// identical across all of them for a fixed key width.
    #[test]
    fn indexed_secondary_mirror_key_size_is_variant_independent() {
        let axes = [IndexAxis::Count];
        let baseline = mirror_cost(EstimatedLayerSizes::AllItems(12, 100, None), &axes);

        // Non-trivial: the mirror opens the secondary merk, derives its
        // prefix (one extra hash) and does a delete + insert.
        assert!(baseline.seek_count > 0, "cost: {baseline:?}");
        assert!(baseline.hash_node_calls > 0, "cost: {baseline:?}");
        assert!(baseline.storage_cost.added_bytes > 0, "cost: {baseline:?}");

        for sizes in [
            EstimatedLayerSizes::AllSubtrees(12, Default::default(), None),
            EstimatedLayerSizes::AllReference(12, 100, None),
            EstimatedLayerSizes::AllItemsWithSumItem(12, 100, None),
            EstimatedLayerSizes::AllReferencesWithSumItem(12, 100, None),
        ] {
            assert_eq!(
                mirror_cost(sizes, &axes),
                baseline,
                "key size 12 must cost the same for {sizes:?}"
            );
        }
    }

    /// A `Mix` layer's key size is the widest of its subtree / item /
    /// reference key sizes, so the mirror cost equals the uniform-layer
    /// cost at that maximum.
    #[test]
    fn indexed_secondary_mirror_mix_layer_uses_widest_key() {
        let axes = [IndexAxis::Sum];
        let mix = EstimatedLayerSizes::Mix {
            subtrees_size: Some((10, Default::default(), None, 1)),
            items_size: Some((20, 100, None, 1)),
            references_size: Some((15, 100, None, 1)),
            items_with_sum_item_size: None,
            references_with_sum_item_size: None,
        };
        assert_eq!(
            mirror_cost(mix, &axes),
            mirror_cost(EstimatedLayerSizes::AllItems(20, 100, None), &axes),
            "Mix must price at its widest key (20), not the narrowest"
        );

        // The subtree entry alone is enough to define the maximum.
        let subtrees_only = EstimatedLayerSizes::Mix {
            subtrees_size: Some((30, Default::default(), None, 1)),
            items_size: None,
            references_size: None,
            items_with_sum_item_size: None,
            references_with_sum_item_size: None,
        };
        assert_eq!(
            mirror_cost(subtrees_only, &axes),
            mirror_cost(
                EstimatedLayerSizes::AllSubtrees(30, Default::default(), None),
                &axes
            ),
        );

        // A reference-only mix likewise.
        let references_only = EstimatedLayerSizes::Mix {
            subtrees_size: None,
            items_size: None,
            references_size: Some((25, 100, None, 1)),
            items_with_sum_item_size: None,
            references_with_sum_item_size: None,
        };
        assert_eq!(
            mirror_cost(references_only, &axes),
            mirror_cost(EstimatedLayerSizes::AllReference(25, 100, None), &axes),
        );

        // No sized component at all falls back to a zero-width key. The
        // sum-item components are deliberately not consulted, so a mix
        // that only describes those is also a zero-width key.
        let empty_mix = EstimatedLayerSizes::Mix {
            subtrees_size: None,
            items_size: None,
            references_size: None,
            items_with_sum_item_size: Some((40, 100, None, 1)),
            references_with_sum_item_size: Some((50, 100, None, 1)),
        };
        assert_eq!(
            mirror_cost(empty_mix, &axes),
            mirror_cost(EstimatedLayerSizes::AllItems(0, 100, None), &axes),
        );
    }

    /// Wider primary keys mean wider secondary keys, and each extra axis
    /// adds one more secondary merk's worth of work: the three-axis cost
    /// is exactly the sum of the three single-axis costs.
    #[test]
    fn indexed_secondary_mirror_scales_with_key_width_and_axis_count() {
        let narrow = mirror_cost(
            EstimatedLayerSizes::AllItems(8, 100, None),
            &[IndexAxis::Count],
        );
        let wide = mirror_cost(
            EstimatedLayerSizes::AllItems(40, 100, None),
            &[IndexAxis::Count],
        );
        assert!(
            wide.storage_cost.added_bytes > narrow.storage_cost.added_bytes,
            "wider primary key must cost more storage: {narrow:?} vs {wide:?}"
        );

        // Two independent contributions to a secondary row's width:
        // the primary key size, and the axis (count/sum sort on 8 bytes,
        // avg on 16, and the avg secondary is a count-AND-sum merk whose
        // nodes carry a wider aggregate).
        let k8_count = narrow.storage_cost.added_bytes;
        let k16_count = mirror_cost(
            EstimatedLayerSizes::AllItems(16, 100, None),
            &[IndexAxis::Count],
        )
        .storage_cost
        .added_bytes;
        let k8_avg = mirror_cost(
            EstimatedLayerSizes::AllItems(8, 100, None),
            &[IndexAxis::Avg],
        )
        .storage_cost
        .added_bytes;
        // +8 bytes of primary key appears in each old/new secondary key
        // and in the canonical sibling-reference payload.
        assert_eq!(k16_count - k8_count, 3 * 8);
        // A 8-byte primary + the avg axis and a 16-byte primary + the
        // count axis both produce a 24-byte secondary key, and since
        // issue #806 EVERY axis is a dual-aggregate
        // ProvableCountProvableSumTree (the count axis mirrors its
        // count_value into the sum half), the merk-node aggregate width
        // is identical across axes. Both rows now use the same canonical
        // `ReferenceWithSumItem` shape. The avg row's shorter primary key
        // saves eight payload bytes, while its wider sort prefix adds eight
        // bytes to each of the old/new secondary keys.
        assert_eq!(k16_count - k8_avg, 8);
        assert_eq!(k8_avg - k8_count, 2 * 8);
        let count_axis = narrow;

        let sizes = EstimatedLayerSizes::AllItems(8, 100, None);
        let all_axes = mirror_cost(sizes, &[IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg]);
        let per_axis = mirror_cost(sizes, &[IndexAxis::Count])
            + mirror_cost(sizes, &[IndexAxis::Sum])
            + mirror_cost(sizes, &[IndexAxis::Avg]);
        assert_eq!(
            all_axes, per_axis,
            "the mirror charges each axis independently"
        );
        assert!(all_axes.seek_count > count_axis.seek_count);

        // No axes at all is free.
        assert_eq!(mirror_cost(sizes, &[]), OperationCost::default());
    }

    /// The secondary key width saturates at `u8::MAX`, but the canonical
    /// reference payload still carries the complete primary key.
    #[test]
    fn indexed_secondary_mirror_reference_payload_grows_after_key_width_saturates() {
        let axes = [IndexAxis::Avg];
        let key_250 = mirror_cost(EstimatedLayerSizes::AllItems(250, 100, None), &axes);
        let key_255 = mirror_cost(EstimatedLayerSizes::AllItems(255, 100, None), &axes);
        assert_eq!(key_250.seek_count, key_255.seek_count);
        assert_eq!(key_250.hash_node_calls, key_255.hash_node_calls);
        assert!(
            key_255.storage_cost.added_bytes > key_250.storage_cost.added_bytes,
            "the secondary key is clamped in both estimates, but the reference payload grows"
        );
    }
}
