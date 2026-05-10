//! Regression tests for `Element::NotSummed` end-to-end behavior.
//!
//! Symmetric to `non_counted_tests.rs`. The wrapper:
//! - May only be inserted into sum-bearing parents (`SumTree`, `BigSumTree`,
//!   `CountSumTree`, `ProvableCountSumTree`).
//! - Inner element must be one of those four sum-tree variants.
//! - Contributes 0 to the parent's running sum; counts still propagate.

#[cfg(test)]
mod tests {
    use grovedb_version::version::GroveVersion;

    use crate::{
        batch::QualifiedGroveDbOp,
        tests::{make_test_grovedb, TEST_LEAF},
        Element,
    };

    /// Establish a sum-tree under `TEST_LEAF/<key>` rooted at the given key
    /// for use as a host parent in the tests below.
    fn make_sum_tree_parent(db: &crate::GroveDb, key: &[u8], grove_version: &GroveVersion) {
        db.insert(
            [TEST_LEAF].as_ref(),
            key,
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert sum tree");
    }

    #[test]
    fn batch_insert_rejects_not_summed_into_normal_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // TEST_LEAF is a normal tree; inserting a NotSummed-wrapped sum tree
        // into it via batch must be rejected, mirroring the per-merk
        // insert guard.
        let ns = Element::new_not_summed(Element::new_sum_tree(None)).expect("wrap ok");
        let op =
            QualifiedGroveDbOp::insert_or_replace_op(vec![TEST_LEAF.to_vec()], b"k".to_vec(), ns);

        let err = db
            .apply_batch(vec![op], None, None, grove_version)
            .unwrap()
            .expect_err("batch insert of NotSummed into NormalTree must fail");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("not-summed") || msg.contains("not_summed"),
            "expected NotSummed parent-type guard error, got: {msg}"
        );
    }

    #[test]
    fn batch_insert_rejects_not_summed_into_count_tree() {
        // CountTree is not sum-bearing, so NotSummed must be rejected.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

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

        let ns = Element::new_not_summed(Element::new_sum_tree(None)).expect("wrap ok");
        let op = QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            b"k".to_vec(),
            ns,
        );

        let err = db
            .apply_batch(vec![op], None, None, grove_version)
            .unwrap()
            .expect_err("batch insert of NotSummed into CountTree must fail");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("not-summed") || msg.contains("not_summed"),
            "expected NotSummed parent-type guard error, got: {msg}"
        );
    }

    #[test]
    fn direct_insert_not_summed_in_sum_tree_excludes_subtree_sum() {
        // A bare SumTree(_, 100) inside a SumTree contributes 100 to the
        // parent's running sum. Wrapped in NotSummed, it contributes 0.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        make_sum_tree_parent(&db, b"outer", grove_version);

        // Bare sum item contributes 7 to the parent's sum.
        db.insert(
            [TEST_LEAF, b"outer"].as_ref(),
            b"plain",
            Element::new_sum_item(7),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert plain sum item");

        // A wrapped SumTree subtree must contribute 0 to the parent's
        // running sum even though it has its own internal sum aggregate.
        let inner_st = Element::new_sum_tree_with_flags_and_sum_value(None, 0, None);
        let wrapped = Element::new_not_summed(inner_st).expect("wrap ok");
        db.insert(
            [TEST_LEAF, b"outer"].as_ref(),
            b"ns",
            wrapped,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert wrapped sum tree");

        // Add a sum item under the wrapped subtree to give it a non-trivial
        // internal sum that must NOT bubble up.
        db.insert(
            [TEST_LEAF, b"outer", b"ns"].as_ref(),
            b"inner_item",
            Element::new_sum_item(99),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert inner sum item");

        // The outer sum tree's running aggregate must be 7 (only `plain`
        // contributes; the wrapped subtree's internal 99 is suppressed).
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
            aggregate.as_sum_i64(),
            7,
            "wrapped sum tree subtree must not contribute to outer sum tree's aggregate; got {:?}",
            aggregate
        );
    }

    #[test]
    fn batch_propagation_preserves_not_summed_wrapper_on_subtree() {
        // A batch that inserts a NotSummed(SumTree) AND writes a child under
        // it forces the propagation path through InsertTreeWithRootHash.
        // The on-disk parent element must come back wrapped, and the outer
        // sum tree's aggregate must exclude the subtree's sum.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Outer sum tree.
        make_sum_tree_parent(&db, b"outer", grove_version);

        // Plain sum item contributes 5.
        db.insert(
            [TEST_LEAF, b"outer"].as_ref(),
            b"plain",
            Element::new_sum_item(5),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert plain");

        // Batch: insert NotSummed(SumTree) under outer + a sum-item child
        // under that wrapped tree, forcing the propagation path.
        let ns_inner = Element::new_not_summed(Element::new_sum_tree(None)).expect("wrap ok");
        let inner_child = Element::new_sum_item(123);
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"outer".to_vec()],
                b"ns".to_vec(),
                ns_inner,
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"outer".to_vec(), b"ns".to_vec()],
                b"child".to_vec(),
                inner_child,
            ),
        ];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch should succeed");

        // The element stored at outer/ns must STILL be NotSummed after
        // propagation.
        let stored = db
            .get_raw(
                grovedb_path::SubtreePath::from(&[TEST_LEAF, b"outer"]),
                b"ns",
                None,
                grove_version,
            )
            .unwrap()
            .expect("get_raw ns");
        assert!(
            matches!(stored, Element::NotSummed(_)),
            "wrapper must survive batch propagation; got {:?}",
            stored
        );

        // The outer sum tree's aggregate must NOT include the wrapped
        // subtree's internal sum (123). Only `plain` (5) should
        // contribute. If propagation dropped the wrapper, the subtree's
        // 123 would bubble up to 128.
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
            aggregate.as_sum_i64(),
            5,
            "not-summed subtree must not contribute to outer sum tree's running sum; got {:?}",
            aggregate
        );
    }

    #[test]
    fn check_subtree_exists_through_not_summed_wrapper() {
        // A NotSummed-wrapped tree at the parent path must satisfy
        // check_subtree_exists, otherwise APIs that gate on it (e.g.
        // inserts into the wrapped tree) would reject paths through
        // wrapped parents.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        make_sum_tree_parent(&db, b"outer", grove_version);

        // A NotSummed(SumTree) inside the outer sum tree.
        db.insert(
            [TEST_LEAF, b"outer"].as_ref(),
            b"ns",
            Element::new_not_summed(Element::new_sum_tree(None)).expect("wrap ok"),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert ns inner");

        // Inserting into the wrapped subtree exercises check_subtree_exists
        // on a path whose parent is `NotSummed(SumTree)` — should succeed.
        db.insert(
            [TEST_LEAF, b"outer", b"ns"].as_ref(),
            b"child",
            Element::new_sum_item(42),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert into wrapped subtree must succeed");
    }
}
