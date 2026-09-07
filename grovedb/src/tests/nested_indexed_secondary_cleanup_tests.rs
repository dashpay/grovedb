//! Issue #888: recursive cleanup must reclaim the per-axis secondary
//! namespaces owned by NESTED indexed trees, not only the primary
//! prefixes the `find_subtrees` walk can see.
//!
//! A nested indexed-tree primary keeps its per-axis secondaries at
//! `Blake3(primary_prefix ‖ axis_tag)` — outside the path-derived prefix
//! space. The direct delete already swept them per descendant; these tests
//! pin the same sweep for every other recursive cleanup route:
//!
//! - full-batch `DeleteTree` cleanup,
//! - partial-batch `DeleteTree` cleanup,
//! - batch cidx safe-subset overwrite cleanup,
//! - the dedicated indexed-tree child overwrite
//!   (`cleanup_dedicated_indexed_child_storage`),
//! - and the direct delete itself (regression guard for the shared-helper
//!   refactor).
//!
//! Prefixes are path-derived, so a survivor row would resurface inside any
//! tree recreated at the same deterministic path, breaking
//! primary-secondary agreement. Each test asserts raw namespace emptiness
//! after cleanup, and the reuse test additionally recreates the path and
//! checks the fresh index only sees fresh rows.

#[cfg(test)]
mod tests {
    use grovedb_element::indexed::IndexAxis;
    use grovedb_merk::tree_type::TreeType;
    use grovedb_path::SubtreePath;
    use grovedb_storage::{rocksdb_storage::RocksDbStorage, RawIterator, Storage, StorageContext};
    use grovedb_version::version::GroveVersion;

    use crate::{
        batch::{BatchApplyOptions, QualifiedGroveDbOp, SubelementsDeletionBehavior},
        operations::delete::DeleteOptions,
        tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
        Element, IndexedAxisEntrySliceExt,
    };

    /// Derive the secondary-namespace prefix for an indexed primary at
    /// `primary_path` and axis `axis`.
    fn secondary_prefix_for_path(primary_path: &[&[u8]], axis: IndexAxis) -> [u8; 32] {
        let subtree: SubtreePath<&[u8]> = primary_path.into();
        let primary_prefix = RocksDbStorage::build_prefix(subtree).unwrap();
        RocksDbStorage::secondary_prefix_for(&primary_prefix, axis.tag()).unwrap()
    }

    /// Whether the storage namespace at `prefix` holds any rows.
    fn namespace_non_empty(db: &TempGroveDb, prefix: [u8; 32]) -> bool {
        let tx = db.start_transaction();
        let ctx = db
            .db
            .get_transactional_storage_context_by_subtree_prefix(prefix, None, &tx)
            .unwrap();
        let mut iter = ctx.raw_iter();
        iter.seek_to_first().unwrap();
        iter.valid().unwrap()
    }

    /// Assert that all three axis namespaces of the indexed primary at
    /// `primary_path` are empty.
    fn assert_all_axis_namespaces_empty(db: &TempGroveDb, primary_path: &[&[u8]], context: &str) {
        for axis in [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg] {
            let prefix = secondary_prefix_for_path(primary_path, axis);
            assert!(
                !namespace_non_empty(db, prefix),
                "{context}: nested indexed secondary namespace (axis {axis:?}) must be empty \
                 after cleanup"
            );
        }
    }

    /// Build `TEST_LEAF/outer` (plain tree) containing a populated nested
    /// PCIT at `TEST_LEAF/outer/nested_pcit`, and return the nested
    /// primary's count-axis secondary prefix (which is non-empty).
    fn make_outer_tree_with_nested_pcit(db: &TempGroveDb, gv: &GroveVersion) -> [u8; 32] {
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::empty_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("insert outer tree");
        db.insert(
            [TEST_LEAF, b"outer"].as_ref(),
            b"nested_pcit",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("insert nested PCIT");
        for key in [b"a" as &[u8], b"b"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"outer", b"nested_pcit"].as_ref(),
                key,
                Element::new_item(b"v".to_vec()),
                None,
                gv,
            )
            .unwrap()
            .expect("populate nested PCIT");
        }
        let count_prefix =
            secondary_prefix_for_path(&[TEST_LEAF, b"outer", b"nested_pcit"], IndexAxis::Count);
        assert!(
            namespace_non_empty(db, count_prefix),
            "baseline: populating the nested PCIT must create count-axis secondary rows"
        );
        count_prefix
    }

    fn assert_verify_clean(db: &TempGroveDb, gv: &GroveVersion) {
        let issues = db
            .verify_grovedb(None, false, true, gv)
            .expect("verify grovedb");
        assert!(issues.is_empty(), "verification issues: {issues:?}");
    }

    // -----------------------------------------------------------------
    // Full-batch DeleteTree cleanup
    // -----------------------------------------------------------------

    #[test]
    fn batch_delete_tree_clears_nested_indexed_secondaries() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        make_outer_tree_with_nested_pcit(&db, gv);

        db.apply_batch(
            vec![QualifiedGroveDbOp::delete_tree_op(
                vec![TEST_LEAF.to_vec()],
                b"outer".to_vec(),
                TreeType::NormalTree,
                SubelementsDeletionBehavior::DeleteChildren,
            )],
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("batch delete outer tree");

        assert_all_axis_namespaces_empty(
            &db,
            &[TEST_LEAF, b"outer", b"nested_pcit"],
            "full batch DeleteTree",
        );
        assert_verify_clean(&db, gv);
    }

    /// Same route, indexed primary two levels below the deletion target and
    /// carrying every axis (PCPSIT with count+sum+avg).
    #[test]
    fn batch_delete_tree_clears_deeply_nested_pcpsit_secondaries() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::empty_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("insert outer tree");
        db.insert(
            [TEST_LEAF, b"outer"].as_ref(),
            b"mid",
            Element::empty_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("insert mid tree");
        let axes: Vec<(u8, Option<Vec<u8>>)> = {
            let mut tags: Vec<u8> = [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg]
                .iter()
                .map(|a| a.tag())
                .collect();
            tags.sort_unstable();
            tags.into_iter().map(|t| (t, None)).collect()
        };
        db.insert(
            [TEST_LEAF, b"outer", b"mid"].as_ref(),
            b"pcpsit",
            Element::empty_provable_count_provable_sum_indexed_tree(axes).expect("canonical axes"),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("insert nested PCPSIT");
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"outer", b"mid", b"pcpsit"].as_ref(),
            b"entry",
            Element::new_item_with_sum_item(b"v".to_vec(), 8),
            None,
            gv,
        )
        .unwrap()
        .expect("populate nested PCPSIT");

        let pcpsit_path: [&[u8]; 4] = [TEST_LEAF, b"outer", b"mid", b"pcpsit"];
        for axis in [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg] {
            assert!(
                namespace_non_empty(&db, secondary_prefix_for_path(&pcpsit_path, axis)),
                "baseline: PCPSIT axis {axis:?} secondary must be populated"
            );
        }

        db.apply_batch(
            vec![QualifiedGroveDbOp::delete_tree_op(
                vec![TEST_LEAF.to_vec()],
                b"outer".to_vec(),
                TreeType::NormalTree,
                SubelementsDeletionBehavior::DeleteChildren,
            )],
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("batch delete outer tree");

        assert_all_axis_namespaces_empty(&db, &pcpsit_path, "full batch DeleteTree (deep PCPSIT)");
        assert_verify_clean(&db, gv);
    }

    // -----------------------------------------------------------------
    // Partial-batch DeleteTree cleanup
    // -----------------------------------------------------------------

    #[test]
    fn partial_batch_delete_tree_clears_nested_indexed_secondaries() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        make_outer_tree_with_nested_pcit(&db, gv);

        db.apply_partial_batch(
            vec![QualifiedGroveDbOp::delete_tree_op(
                vec![TEST_LEAF.to_vec()],
                b"outer".to_vec(),
                TreeType::NormalTree,
                SubelementsDeletionBehavior::DeleteChildren,
            )],
            None,
            |_cost, _left_over_ops| Ok(vec![]),
            None,
            gv,
        )
        .unwrap()
        .expect("partial batch delete outer tree");

        assert_all_axis_namespaces_empty(
            &db,
            &[TEST_LEAF, b"outer", b"nested_pcit"],
            "partial batch DeleteTree",
        );
        assert_verify_clean(&db, gv);
    }

    // -----------------------------------------------------------------
    // Batch cidx safe-subset overwrite cleanup
    // -----------------------------------------------------------------

    /// Overwriting a populated PCIT with an empty PCIT (the safe subset)
    /// must also clear the secondaries of an indexed primary NESTED under
    /// one of the replaced PCIT's tree-typed children.
    #[test]
    fn batch_cidx_overwrite_clears_nested_indexed_secondaries() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("insert PCIT");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"row",
            Element::empty_tree(),
            None,
            gv,
        )
        .unwrap()
        .expect("insert plain-tree row into PCIT");
        db.insert(
            [TEST_LEAF, b"cidx", b"row"].as_ref(),
            b"nested_pcit",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("insert nested PCIT under the row subtree");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx", b"row", b"nested_pcit"].as_ref(),
            b"e",
            Element::new_item(b"v".to_vec()),
            None,
            gv,
        )
        .unwrap()
        .expect("populate nested PCIT");

        let nested_path: [&[u8]; 4] = [TEST_LEAF, b"cidx", b"row", b"nested_pcit"];
        assert!(
            namespace_non_empty(
                &db,
                secondary_prefix_for_path(&nested_path, IndexAxis::Count)
            ),
            "baseline: nested PCIT count secondary must be populated"
        );

        // Indexed -> empty indexed is the classifier's safe subset; the
        // post-apply pass must reclaim the whole old namespace family.
        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"cidx".to_vec(),
                Element::empty_provable_count_indexed_tree(),
            )],
            Some(BatchApplyOptions {
                validate_insertion_does_not_override: false,
                validate_insertion_does_not_override_tree: false,
                ..Default::default()
            }),
            None,
            gv,
        )
        .unwrap()
        .expect("indexed -> empty indexed overwrite");

        assert_all_axis_namespaces_empty(&db, &nested_path, "batch cidx overwrite (nested)");
        // The replaced primary's own secondaries stay covered too.
        assert_all_axis_namespaces_empty(
            &db,
            &[TEST_LEAF, b"cidx"],
            "batch cidx overwrite (top level)",
        );
        assert_verify_clean(&db, gv);
    }

    // -----------------------------------------------------------------
    // Dedicated indexed-child overwrite cleanup
    // -----------------------------------------------------------------

    /// `insert_into_count_indexed_tree` overwriting a tree-typed child must
    /// clear the secondaries of an indexed primary nested inside the
    /// replaced child's subtree.
    #[test]
    fn dedicated_indexed_overwrite_clears_nested_indexed_secondaries() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("insert PCIT");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"row",
            Element::empty_tree(),
            None,
            gv,
        )
        .unwrap()
        .expect("insert plain-tree row into PCIT");
        db.insert(
            [TEST_LEAF, b"cidx", b"row"].as_ref(),
            b"nested_pcit",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("insert nested PCIT under the row subtree");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx", b"row", b"nested_pcit"].as_ref(),
            b"e",
            Element::new_item(b"v".to_vec()),
            None,
            gv,
        )
        .unwrap()
        .expect("populate nested PCIT");

        let nested_path: [&[u8]; 4] = [TEST_LEAF, b"cidx", b"row", b"nested_pcit"];
        assert!(
            namespace_non_empty(
                &db,
                secondary_prefix_for_path(&nested_path, IndexAxis::Count)
            ),
            "baseline: nested PCIT count secondary must be populated"
        );

        // Dedicated-path overwrite of the tree-typed child.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"row",
            Element::empty_tree(),
            None,
            gv,
        )
        .unwrap()
        .expect("dedicated overwrite of the row subtree");

        assert_all_axis_namespaces_empty(&db, &nested_path, "dedicated indexed overwrite");
        assert_verify_clean(&db, gv);
    }

    // -----------------------------------------------------------------
    // Direct delete (regression guard for the shared-helper refactor)
    // -----------------------------------------------------------------

    #[test]
    fn direct_delete_still_clears_nested_indexed_secondaries() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        make_outer_tree_with_nested_pcit(&db, gv);

        db.delete(
            [TEST_LEAF].as_ref(),
            b"outer",
            Some(DeleteOptions {
                allow_deleting_non_empty_trees: true,
                deleting_non_empty_trees_returns_error: false,
                ..Default::default()
            }),
            None,
            gv,
        )
        .unwrap()
        .expect("direct delete outer tree");

        assert_all_axis_namespaces_empty(
            &db,
            &[TEST_LEAF, b"outer", b"nested_pcit"],
            "direct delete",
        );
        assert_verify_clean(&db, gv);
    }

    // -----------------------------------------------------------------
    // Path reuse after cleanup
    // -----------------------------------------------------------------

    /// After the batch cleanup, recreating the SAME deterministic path must
    /// produce an index that reflects only the fresh rows — the failure
    /// mode of #888 was stale secondary rows resurfacing here.
    #[test]
    fn path_reuse_after_batch_delete_sees_only_fresh_rows() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        make_outer_tree_with_nested_pcit(&db, gv);

        db.apply_batch(
            vec![QualifiedGroveDbOp::delete_tree_op(
                vec![TEST_LEAF.to_vec()],
                b"outer".to_vec(),
                TreeType::NormalTree,
                SubelementsDeletionBehavior::DeleteChildren,
            )],
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("batch delete outer tree");

        // Recreate the exact same path and repopulate with ONE fresh row.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::empty_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("recreate outer tree");
        db.insert(
            [TEST_LEAF, b"outer"].as_ref(),
            b"nested_pcit",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("recreate nested PCIT");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer", b"nested_pcit"].as_ref(),
            b"fresh",
            Element::new_item(b"v".to_vec()),
            None,
            gv,
        )
        .unwrap()
        .expect("populate recreated PCIT");

        let rows = db
            .indexed_count_top_k(
                [TEST_LEAF, b"outer", b"nested_pcit"].as_ref(),
                10,
                true,
                None,
                gv,
            )
            .unwrap()
            .expect("top_k on recreated PCIT");
        assert_eq!(
            rows.key_pairs(),
            vec![(1u64, b"fresh".to_vec())],
            "the recreated index must only contain the fresh row — stale rows from the \
             deleted tree must not resurface"
        );
        assert_verify_clean(&db, gv);
    }
}
