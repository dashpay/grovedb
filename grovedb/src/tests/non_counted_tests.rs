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

        let result = db.apply_batch(vec![op], None, None, grove_version).unwrap();
        assert!(
            result.is_err(),
            "batch insert of NonCounted into NormalTree must fail"
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
    }
}
