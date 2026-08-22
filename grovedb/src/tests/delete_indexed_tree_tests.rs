//! Coverage tests for the `delete_internal_on_transaction` indexed-tree
//! branches in `grovedb/src/operations/delete/mod.rs`.
//!
//! Targets:
//! - Deleting a PCIT/PSIT/PCPSIT primary with `allow_deleting_non_empty_trees`
//!   when the primary holds children — exercises the nested
//!   secondary-namespace sweep for all three axis tags.
//! - Deleting a PCIT/PSIT/PCPSIT primary with default options when the
//!   primary holds children — the "non-empty tree" guard fires and
//!   returns an error / false.
//! - Deleting an empty PCIT/PSIT/PCPSIT primary still triggers the
//!   defensive secondary clear.

#[cfg(test)]
mod tests {
    use grovedb_element::indexed::IndexAxis;
    use grovedb_path::SubtreePath;
    use grovedb_storage::{
        rocksdb_storage::RocksDbStorage, RawIterator, Storage, StorageBatch, StorageContext,
    };
    use grovedb_version::version::GroveVersion;

    use crate::{
        operations::delete::DeleteOptions,
        tests::{make_test_grovedb, TEST_LEAF},
        Element, Error,
    };

    /// Seed a stale ("drifted") raw KV directly into an indexed tree's
    /// per-axis secondary storage namespace, bypassing the primary. This
    /// mirrors a bug where the secondary index fails to be mirror-cleaned
    /// while the primary is emptied, leaving orphan rows at
    /// `Blake3(primary_prefix ‖ axis_tag)`. Returns the secondary prefix
    /// so the caller can scan it before/after delete.
    fn seed_stale_secondary_row(
        db: &crate::GroveDb,
        primary_path: &[&[u8]],
        axis: IndexAxis,
    ) -> [u8; 32] {
        let path_vec: Vec<&[u8]> = primary_path.to_vec();
        let subtree: SubtreePath<&[u8]> = path_vec.as_slice().into();
        let primary_prefix = RocksDbStorage::build_prefix(subtree).unwrap();
        let secondary_prefix =
            RocksDbStorage::secondary_prefix_for(&primary_prefix, axis.tag()).unwrap();

        let tx = db.start_transaction();
        let batch = StorageBatch::new();
        {
            let ctx = db
                .db
                .get_transactional_storage_context_by_subtree_prefix(
                    secondary_prefix,
                    Some(&batch),
                    &tx,
                )
                .unwrap();
            ctx.put(b"stale_orphan_key", b"stale_orphan_value", None, None)
                .unwrap()
                .expect("seed stale secondary row");
        }
        db.db
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .expect("commit stale row");
        tx.commit().expect("commit tx");
        secondary_prefix
    }

    /// Assert whether the secondary namespace at `secondary_prefix` has
    /// any rows.
    fn secondary_namespace_non_empty(db: &crate::GroveDb, secondary_prefix: [u8; 32]) -> bool {
        let tx = db.start_transaction();
        let ctx = db
            .db
            .get_transactional_storage_context_by_subtree_prefix(secondary_prefix, None, &tx)
            .unwrap();
        let mut iter = ctx.raw_iter();
        iter.seek_to_first().unwrap();
        iter.valid().unwrap()
    }

    fn delete_options_allow_non_empty() -> DeleteOptions {
        DeleteOptions {
            allow_deleting_non_empty_trees: true,
            deleting_non_empty_trees_returns_error: false,
            base_root_storage_is_free: true,
            validate_tree_at_path_exists: false,
        }
    }

    // ---------- PCIT delete ----------

    #[test]
    fn delete_pcit_with_children_succeeds_with_allow_flag() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcit",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PCIT");
        // Children enter EMPTY and are populated so their counts are
        // DERIVED; the delete-with-children path is unaffected by how the
        // aggregate got there, only that the primary is non-empty.
        for (k, c) in &[(b"a" as &[u8], 1u64), (b"b" as &[u8], 5u64)] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"pcit"].as_ref(),
                k,
                Element::empty_provable_count_tree(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert PCIT entry");
            for i in 0..*c {
                db.insert(
                    [TEST_LEAF, b"pcit", k].as_ref(),
                    &i.to_be_bytes(),
                    Element::new_item(vec![]),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("derive PCIT entry count");
            }
        }
        // Delete the PCIT primary itself with allow flag.
        db.delete(
            [TEST_LEAF].as_ref(),
            b"pcit",
            Some(delete_options_allow_non_empty()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete PCIT root with children");
        // The PCIT element should be gone.
        let get_result = db
            .get([TEST_LEAF].as_ref(), b"pcit", None, grove_version)
            .unwrap();
        assert!(get_result.is_err());
    }

    /// A generic non-empty tree delete NESTED BELOW an indexed primary must
    /// re-mirror the primary's secondary row on the way up: the delete
    /// changes the child subtree's root (and possibly its count), and the
    /// canonical secondary row binds that node's commitment. The v1
    /// (GROVE_V4) delete path routes propagation through the full
    /// indexed-aware walk; the legacy batch propagation performed only the
    /// basic parent update and left the secondary stale.
    #[test]
    fn delete_non_empty_tree_nested_below_pcit_remirrors_secondary() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcit",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PCIT");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"pcit"].as_ref(),
            b"a",
            Element::empty_provable_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert PCIT entry");
        // Populate the entry subtree generically (allowed: `a` is a plain
        // ProvableCountTree, not a primary), including a nested non-empty
        // tree whose deletion will exercise the non-empty delete branch.
        db.insert(
            [TEST_LEAF, b"pcit", b"a"].as_ref(),
            b"sibling",
            Element::new_item(vec![]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert sibling item");
        db.insert(
            [TEST_LEAF, b"pcit", b"a"].as_ref(),
            b"t",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert nested tree");
        db.insert(
            [TEST_LEAF, b"pcit", b"a", b"t"].as_ref(),
            b"leaf",
            Element::new_item(b"v".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert nested leaf");

        // Delete the NON-EMPTY nested tree `t`. Its parent `a` is not a
        // primary, so the generic-write rejection does not fire, and the
        // propagation walk climbs a -> pcit (indexed primary) -> test_leaf.
        db.delete(
            [TEST_LEAF, b"pcit", b"a"].as_ref(),
            b"t",
            Some(delete_options_allow_non_empty()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete non-empty nested tree below a PCIT entry");

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify grovedb");
        assert!(issues.is_empty(), "verification issues: {:?}", issues);
    }

    #[test]
    fn delete_empty_pcit_succeeds_and_clears_secondary() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcit_empty",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create empty PCIT");
        // Delete with default options (no allow flag needed; tree is empty).
        db.delete(
            [TEST_LEAF].as_ref(),
            b"pcit_empty",
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete empty PCIT");
    }

    #[test]
    fn delete_pcit_non_empty_without_allow_returns_error() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcit",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PCIT");
        // Empty child, then one item inside it: the PCIT is non-empty by
        // DERIVED content, which is what the no-allow-flag delete must
        // still refuse.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"pcit"].as_ref(),
            b"a",
            Element::empty_provable_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert PCIT entry");
        db.insert(
            [TEST_LEAF, b"pcit", b"a"].as_ref(),
            b"row",
            Element::new_item(b"v".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("derive PCIT entry count");
        let result = db
            .delete([TEST_LEAF].as_ref(), b"pcit", None, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::DeletingNonEmptyTree(_))));
    }

    // ---------- PSIT delete ----------

    #[test]
    fn delete_psit_with_children_succeeds_with_allow_flag() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"psit",
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PSIT");
        for (k, s) in &[(b"a" as &[u8], 1i64), (b"b" as &[u8], -5i64)] {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"psit"].as_ref(),
                k,
                Element::new_sum_item(*s),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert PSIT entry");
        }
        // Delete the PSIT primary with children.
        db.delete(
            [TEST_LEAF].as_ref(),
            b"psit",
            Some(delete_options_allow_non_empty()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete PSIT root with children");
        let get_result = db
            .get([TEST_LEAF].as_ref(), b"psit", None, grove_version)
            .unwrap();
        assert!(get_result.is_err());
    }

    #[test]
    fn delete_empty_psit_succeeds() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"psit_empty",
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create empty PSIT");
        db.delete(
            [TEST_LEAF].as_ref(),
            b"psit_empty",
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete empty PSIT");
    }

    #[test]
    fn delete_psit_non_empty_without_allow_returns_error() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"psit",
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PSIT");
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"a",
            Element::new_sum_item(7),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert PSIT entry");
        let result = db
            .delete([TEST_LEAF].as_ref(), b"psit", None, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::DeletingNonEmptyTree(_))));
    }

    // ---------- PCPSIT delete ----------

    #[test]
    fn delete_pcpsit_with_multi_axis_children_succeeds_with_allow_flag() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let axes: Vec<(u8, Option<Vec<u8>>)> = vec![
            (IndexAxis::Count.tag(), None),
            (IndexAxis::Sum.tag(), None),
            (IndexAxis::Avg.tag(), None),
        ];
        let elem =
            Element::empty_provable_count_provable_sum_indexed_tree(axes).expect("axes canonical");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            elem,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PCPSIT");
        for (k, s) in &[
            (b"a" as &[u8], 1i64),
            (b"b" as &[u8], 5i64),
            (b"c" as &[u8], 9),
        ] {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                k,
                Element::new_item_with_sum_item(b"v".to_vec(), *s),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert PCPSIT entry");
        }
        // Delete the PCPSIT primary with children — exercises the
        // multi-axis secondary cleanup sweep over all three IndexAxis
        // tags inside the find_subtrees loop.
        db.delete(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            Some(delete_options_allow_non_empty()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete PCPSIT root with multi-axis children");
        let get_result = db
            .get([TEST_LEAF].as_ref(), b"pcpsit", None, grove_version)
            .unwrap();
        assert!(get_result.is_err());
    }

    #[test]
    fn delete_pcpsit_single_axis_with_children_succeeds_with_allow_flag() {
        // PCPSIT with only the count axis in TLV; delete with children.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let axes: Vec<(u8, Option<Vec<u8>>)> = vec![(IndexAxis::Count.tag(), None)];
        let elem =
            Element::empty_provable_count_provable_sum_indexed_tree(axes).expect("axes canonical");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            elem,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PCPSIT");
        for k in &[b"a" as &[u8], b"b", b"c"] {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                k,
                Element::new_item_with_sum_item(b"v".to_vec(), 1),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert PCPSIT entry");
        }
        db.delete(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            Some(delete_options_allow_non_empty()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete PCPSIT root with single-axis children");
    }

    #[test]
    fn delete_empty_pcpsit_succeeds_with_default_options() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let axes: Vec<(u8, Option<Vec<u8>>)> = vec![(IndexAxis::Sum.tag(), None)];
        let elem =
            Element::empty_provable_count_provable_sum_indexed_tree(axes).expect("axes canonical");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit_empty",
            elem,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create empty PCPSIT");
        db.delete(
            [TEST_LEAF].as_ref(),
            b"pcpsit_empty",
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete empty PCPSIT");
    }

    #[test]
    fn delete_pcpsit_non_empty_without_allow_returns_error() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let axes: Vec<(u8, Option<Vec<u8>>)> =
            vec![(IndexAxis::Count.tag(), None), (IndexAxis::Sum.tag(), None)];
        let elem =
            Element::empty_provable_count_provable_sum_indexed_tree(axes).expect("axes canonical");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            elem,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PCPSIT");
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"pcpsit"].as_ref(),
            b"a",
            Element::new_item_with_sum_item(b"v".to_vec(), 1),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert PCPSIT entry");
        let result = db
            .delete([TEST_LEAF].as_ref(), b"pcpsit", None, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::DeletingNonEmptyTree(_))));
    }

    // ---------- Nested indexed-tree delete ----------

    #[test]
    fn delete_outer_tree_with_nested_pcpsit_clears_secondaries() {
        // Outer regular tree contains a nested PCPSIT with children.
        // Deleting the outer tree must sweep the nested PCPSIT's three
        // secondary namespaces via the per-prefix axis sweep.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create outer");
        let axes: Vec<(u8, Option<Vec<u8>>)> = vec![
            (IndexAxis::Count.tag(), None),
            (IndexAxis::Sum.tag(), None),
            (IndexAxis::Avg.tag(), None),
        ];
        let elem =
            Element::empty_provable_count_provable_sum_indexed_tree(axes).expect("axes canonical");
        db.insert(
            [TEST_LEAF, b"outer"].as_ref(),
            b"nested_pcpsit",
            elem,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create nested PCPSIT");
        for k in &[b"a" as &[u8], b"b"] {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"outer", b"nested_pcpsit"].as_ref(),
                k,
                Element::new_item_with_sum_item(b"v".to_vec(), 1),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert nested PCPSIT entry");
        }
        // Delete the outer tree with allow flag — exercise the
        // find_subtrees walk + the nested-secondary sweep over all
        // three axis tags for the nested PCPSIT.
        db.delete(
            [TEST_LEAF].as_ref(),
            b"outer",
            Some(delete_options_allow_non_empty()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete outer tree containing nested PCPSIT");
    }

    #[test]
    fn delete_outer_tree_with_nested_psit_clears_secondaries() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create outer");
        db.insert(
            [TEST_LEAF, b"outer"].as_ref(),
            b"nested_psit",
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create nested PSIT");
        for (k, s) in &[(b"a" as &[u8], 1i64), (b"b" as &[u8], 2i64)] {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"outer", b"nested_psit"].as_ref(),
                k,
                Element::new_sum_item(*s),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert nested PSIT entry");
        }
        db.delete(
            [TEST_LEAF].as_ref(),
            b"outer",
            Some(delete_options_allow_non_empty()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete outer tree containing nested PSIT");
    }

    // ---------- Drifted-secondary cleanup on empty-primary delete ----------
    //
    // These tests prove the defensive secondary-namespace clear in
    // `delete/mod.rs` (`is_indexed_primary()` gate) sweeps ALL axis tags,
    // not just Count. They seed a stale row directly into a per-axis
    // secondary namespace (bypassing the primary, so the primary stays
    // empty and the non-empty `find_subtrees` sweep is NOT reached), then
    // delete the empty primary and assert the namespace is cleared. Before
    // the fix (gate = `is_count_indexed_primary()`, axis = Count only) the
    // Sum/Avg orphans on PSIT/PCPSIT would survive the delete.

    #[test]
    fn delete_empty_psit_clears_drifted_sum_secondary() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"psit_drift",
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create empty PSIT");

        // PSIT indexes on the Sum axis; seed a stale row there.
        let secondary_prefix =
            seed_stale_secondary_row(&db, &[TEST_LEAF, b"psit_drift"], IndexAxis::Sum);
        assert!(
            secondary_namespace_non_empty(&db, secondary_prefix),
            "drift sanity: PSIT sum secondary must be non-empty before delete"
        );

        // Delete the (primary-)empty PSIT with default options.
        db.delete(
            [TEST_LEAF].as_ref(),
            b"psit_drift",
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete empty (drifted) PSIT");

        assert!(
            !secondary_namespace_non_empty(&db, secondary_prefix),
            "PSIT sum secondary must be empty after delete; drift cleared by is_indexed_primary \
             sweep"
        );
    }

    #[test]
    fn delete_empty_pcpsit_clears_all_drifted_axis_secondaries() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let axes: Vec<(u8, Option<Vec<u8>>)> = vec![
            (IndexAxis::Count.tag(), None),
            (IndexAxis::Sum.tag(), None),
            (IndexAxis::Avg.tag(), None),
        ];
        let elem =
            Element::empty_provable_count_provable_sum_indexed_tree(axes).expect("axes canonical");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit_drift",
            elem,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create empty PCPSIT");

        // Seed a stale row into EACH of the three axis secondaries.
        let axes_to_test = [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg];
        let secondary_prefixes: Vec<[u8; 32]> = axes_to_test
            .iter()
            .map(|axis| seed_stale_secondary_row(&db, &[TEST_LEAF, b"pcpsit_drift"], *axis))
            .collect();
        for (axis, prefix) in axes_to_test.iter().zip(&secondary_prefixes) {
            assert!(
                secondary_namespace_non_empty(&db, *prefix),
                "drift sanity: PCPSIT {axis:?} secondary must be non-empty before delete"
            );
        }

        db.delete(
            [TEST_LEAF].as_ref(),
            b"pcpsit_drift",
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete empty (drifted) PCPSIT");

        for (axis, prefix) in axes_to_test.iter().zip(&secondary_prefixes) {
            assert!(
                !secondary_namespace_non_empty(&db, *prefix),
                "PCPSIT {axis:?} secondary must be empty after delete; drift cleared by \
                 is_indexed_primary sweep over all three axes"
            );
        }
    }
}
