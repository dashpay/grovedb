//! Regression tests for `Element::NonCounted` end-to-end behavior.
//!
//! These cover the issues found in the Codex review of PR #654:
//! - Wrapped references resolve via the get path (P2 #5).
//! - Batch insert rejects NonCounted into non-count-bearing parents (P2 #4).
//! - Batch propagation preserves the wrapper through
//!   `InsertTreeWithRootHash` / `InsertNonMerkTree` so the on-disk parent
//!   element keeps its wrapper byte and the count aggregate excludes the
//!   subtree (P1 #2).

#[cfg(test)]
mod tests {
    use grovedb_version::version::GroveVersion;

    use crate::{
        batch::QualifiedGroveDbOp,
        reference_path::ReferencePathType,
        tests::{make_test_grovedb, TEST_LEAF},
        Element,
    };

    #[test]
    fn get_follows_non_counted_reference() {
        // A NonCounted-wrapped reference inside a count tree should still
        // be followed by `get`, returning the referenced item's value
        // rather than the bare reference element.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Set up a count tree under TEST_LEAF.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"ct",
            Element::empty_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert ct");

        // Insert the target item the reference will point at, also inside
        // the count tree.
        let target_value = b"target-value".to_vec();
        db.insert(
            [TEST_LEAF, b"ct"].as_ref(),
            b"target",
            Element::new_item(target_value.clone()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert target");

        // Insert a NonCounted-wrapped reference that points at the target.
        let reference = Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"ct".to_vec(),
            b"target".to_vec(),
        ]));
        let nc_reference = Element::new_non_counted(reference).expect("wrap ok");
        db.insert(
            [TEST_LEAF, b"ct"].as_ref(),
            b"nc_ref",
            nc_reference,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert nc reference");

        // get() resolves through the wrapper and returns the target item.
        let resolved = db
            .get([TEST_LEAF, b"ct"].as_ref(), b"nc_ref", None, grove_version)
            .unwrap()
            .expect("get should resolve through NonCounted reference");
        assert_eq!(
            resolved,
            Element::new_item(target_value),
            "wrapped reference must resolve to the referenced item"
        );
    }

    #[test]
    fn batch_insert_rejects_non_counted_into_normal_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // TEST_LEAF is a normal tree; inserting a NonCounted-wrapped item
        // into it via batch must be rejected, mirroring the per-merk
        // insert guard.
        let nc_item = Element::new_non_counted(Element::new_item(b"x".to_vec())).expect("wrap ok");
        let op = QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"k".to_vec(),
            nc_item,
        );

        let err = db
            .apply_batch(vec![op], None, None, grove_version)
            .unwrap()
            .expect_err("batch insert of NonCounted into NormalTree must fail");
        // Make sure we're catching the parent-type guard, not some unrelated
        // batch validation error.
        let msg = format!("{err:?}");
        assert!(
            msg.contains("non-counted"),
            "expected NonCounted parent-type guard error, got: {msg}"
        );
    }

    #[test]
    fn batch_propagation_preserves_non_counted_wrapper_on_subtree() {
        // A batch that inserts a NonCounted(CountTree) AND writes a child
        // under it forces the propagation path through
        // InsertTreeWithRootHash. The on-disk parent element must come
        // back wrapped, and the outer count tree's aggregate must
        // exclude the subtree's count.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Outer count tree (counts all children that aren't NonCounted).
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::empty_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert outer count tree");

        // One ordinary child contributes 1 to outer's count.
        db.insert(
            [TEST_LEAF, b"outer"].as_ref(),
            b"plain",
            Element::new_item(b"a".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert plain child");

        // Now batch: insert a NonCounted(CountTree) under outer AND a
        // child under that NonCounted tree in the same batch. This
        // exercises the propagation path that previously dropped the
        // wrapper.
        let nc_inner = Element::new_non_counted(Element::empty_count_tree()).expect("wrap ok");
        let inner_child = Element::new_item(b"b".to_vec());
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"outer".to_vec()],
                b"nc".to_vec(),
                nc_inner,
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"outer".to_vec(), b"nc".to_vec()],
                b"child".to_vec(),
                inner_child,
            ),
        ];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch should succeed");

        // The element stored at outer/nc must STILL be NonCounted after
        // propagation (the bug was: wrapper was silently dropped).
        let stored = db
            .get_raw(
                grovedb_path::SubtreePath::from(&[TEST_LEAF, b"outer"]),
                b"nc",
                None,
                grove_version,
            )
            .unwrap()
            .expect("get_raw nc");
        assert!(
            matches!(stored, Element::NonCounted(_)),
            "wrapper must survive batch propagation; got {:?}",
            stored
        );

        // The outer count tree's aggregate must NOT include the
        // NonCounted subtree. Only `plain` should count, so the outer's
        // root aggregate count is 1. (If propagation dropped the wrapper,
        // the subtree would be counted as a regular tree → 2.)
        use grovedb_storage::StorageBatch;
        let batch = StorageBatch::new();
        let tx = db.start_transaction();
        let outer_merk = db
            .open_transactional_merk_at_path(
                [TEST_LEAF, b"outer"].as_ref().into(),
                &tx,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("open outer merk");
        let aggregate = outer_merk
            .aggregate_data()
            .expect("read outer aggregate data");
        assert_eq!(
            aggregate.as_count_u64(),
            1,
            "non-counted subtree must not contribute to outer count tree's aggregate; got {:?}",
            aggregate
        );
    }

    #[test]
    fn typed_mmr_api_works_through_non_counted_wrapper() {
        // A NonCounted-wrapped MmrTree should still work with the typed
        // mmr_tree_* methods. Without the underlying() lookups in
        // operations/mmr_tree.rs, the typed APIs would reject wrapped
        // trees as "not an MMR tree".
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Outer count tree to host the wrapped MmrTree.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"ct",
            Element::empty_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert ct");

        // Wrap an empty MmrTree in NonCounted and insert it inside the
        // count tree. The wrapper suppresses the count contribution.
        let wrapped_mmr = Element::new_non_counted(Element::empty_mmr_tree()).expect("wrap ok");
        db.insert(
            [TEST_LEAF, b"ct"].as_ref(),
            b"mmr",
            wrapped_mmr,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert wrapped mmr");

        // mmr_tree_leaf_count must look through the wrapper.
        let count = db
            .mmr_tree_leaf_count([TEST_LEAF, b"ct"].as_ref(), b"mmr", None, grove_version)
            .unwrap()
            .expect("leaf count must succeed for wrapped MmrTree");
        assert_eq!(count, 0, "fresh MMR has zero leaves");
    }

    #[test]
    fn check_subtree_exists_through_non_counted_wrapper() {
        // A NonCounted-wrapped tree at the parent path must satisfy
        // check_subtree_exists, otherwise APIs that gate on it (e.g.
        // inserts into the wrapped tree) would reject paths through
        // wrapped parents.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Outer count tree under TEST_LEAF.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"ct",
            Element::empty_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert ct");

        // A NonCounted(CountTree) inside the outer count tree.
        db.insert(
            [TEST_LEAF, b"ct"].as_ref(),
            b"nc_ct",
            Element::new_non_counted(Element::empty_count_tree()).expect("wrap ok"),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert nc inner");

        // Inserting into the wrapped subtree exercises check_subtree_exists
        // on a path whose parent is `NonCounted(CountTree)` — should succeed.
        db.insert(
            [TEST_LEAF, b"ct", b"nc_ct"].as_ref(),
            b"child",
            Element::new_item(b"v".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert into wrapped subtree must succeed");
    }
}
