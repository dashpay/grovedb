//! Regression tests for `Element::NotCountedOrSummed` end-to-end behavior.
//!
//! Symmetric to `not_summed_tests.rs` / `non_counted_tests.rs`. The wrapper:
//! - May only be inserted into trees that bear BOTH a count and a sum
//!   (`CountSumTree`, `ProvableCountSumTree`).
//! - Inner element must be one of the four sum-tree variants (`SumTree`,
//!   `BigSumTree`, `CountSumTree`, `ProvableCountSumTree`).
//! - Contributes 0 to BOTH the parent's running sum AND its count. Internal
//!   aggregates of the wrapped tree itself remain intact.

#[cfg(test)]
mod tests {
    use grovedb_storage::StorageBatch;
    use grovedb_version::version::GroveVersion;

    use crate::{
        batch::QualifiedGroveDbOp,
        tests::{make_test_grovedb, TEST_LEAF},
        Element,
    };

    /// Establish a count-sum tree under `TEST_LEAF/<key>` for use as a host
    /// parent in the tests below.
    fn make_count_sum_tree_parent(db: &crate::GroveDb, key: &[u8], grove_version: &GroveVersion) {
        db.insert(
            [TEST_LEAF].as_ref(),
            key,
            Element::empty_count_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert count sum tree");
    }

    /// Establish a provable count-sum tree under `TEST_LEAF/<key>`.
    fn make_provable_count_sum_tree_parent(
        db: &crate::GroveDb,
        key: &[u8],
        grove_version: &GroveVersion,
    ) {
        db.insert(
            [TEST_LEAF].as_ref(),
            key,
            Element::empty_provable_count_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert provable count sum tree");
    }

    #[test]
    fn batch_insert_rejects_not_counted_or_summed_into_normal_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // TEST_LEAF is a normal tree; the wrapper must be rejected there.
        let w = Element::new_not_counted_or_summed(Element::new_sum_tree(None)).expect("wrap ok");
        let op =
            QualifiedGroveDbOp::insert_or_replace_op(vec![TEST_LEAF.to_vec()], b"k".to_vec(), w);

        let err = db
            .apply_batch(vec![op], None, None, grove_version)
            .unwrap()
            .expect_err("batch insert into NormalTree must fail");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("not-counted-or-summed") || msg.contains("not_counted_or_summed"),
            "expected NotCountedOrSummed parent-type guard error, got: {msg}"
        );
    }

    #[test]
    fn batch_insert_rejects_not_counted_or_summed_into_count_tree() {
        // CountTree bears a count but no sum, so the wrapper has nothing to
        // suppress on the sum axis. The parent-type guard must reject it.
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

        let w = Element::new_not_counted_or_summed(Element::new_sum_tree(None)).expect("wrap ok");
        let op = QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            b"k".to_vec(),
            w,
        );

        let err = db
            .apply_batch(vec![op], None, None, grove_version)
            .unwrap()
            .expect_err("batch insert into CountTree must fail");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("not-counted-or-summed") || msg.contains("not_counted_or_summed"),
            "expected NotCountedOrSummed parent-type guard error, got: {msg}"
        );
    }

    #[test]
    fn batch_insert_rejects_not_counted_or_summed_into_sum_tree() {
        // SumTree bears a sum but no count, so the wrapper has nothing to
        // suppress on the count axis. The parent-type guard must reject it.
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
        .expect("insert st");

        let w = Element::new_not_counted_or_summed(Element::new_sum_tree(None)).expect("wrap ok");
        let op = QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            b"k".to_vec(),
            w,
        );

        let err = db
            .apply_batch(vec![op], None, None, grove_version)
            .unwrap()
            .expect_err("batch insert into SumTree must fail");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("not-counted-or-summed") || msg.contains("not_counted_or_summed"),
            "expected NotCountedOrSummed parent-type guard error, got: {msg}"
        );
    }

    #[test]
    fn direct_insert_not_counted_or_summed_in_count_sum_tree_excludes_both_axes() {
        // A bare SumTree inside a CountSumTree contributes (1, internal_sum)
        // to the parent's (count, sum). Wrapped in NotCountedOrSummed, it
        // must contribute (0, 0).
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        make_count_sum_tree_parent(&db, b"outer", grove_version);

        // A plain sum item contributes (1, 5).
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

        // A NotCountedOrSummed(SumTree(_, 100)) must contribute (0, 0).
        let inner = Element::new_sum_tree_with_flags_and_sum_value(None, 0, None);
        let wrapped = Element::new_not_counted_or_summed(inner).expect("wrap ok");
        db.insert(
            [TEST_LEAF, b"outer"].as_ref(),
            b"w",
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
            [TEST_LEAF, b"outer", b"w"].as_ref(),
            b"inner_item",
            Element::new_sum_item(99),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert inner sum item");

        // The outer count-sum tree's aggregate must be (count=1, sum=5).
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
            "wrapped subtree must not contribute to outer count; got {:?}",
            aggregate
        );
        assert_eq!(
            aggregate.as_sum_i64(),
            5,
            "wrapped subtree must not contribute to outer sum; got {:?}",
            aggregate
        );
    }

    #[test]
    fn direct_insert_not_counted_or_summed_in_provable_count_sum_tree_excludes_both_axes() {
        // Same scenario but with a ProvableCountSumTree parent.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        make_provable_count_sum_tree_parent(&db, b"outer", grove_version);

        // A plain item contributes (1, 0).
        db.insert(
            [TEST_LEAF, b"outer"].as_ref(),
            b"plain",
            Element::new_item(b"x".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert plain item");

        // NotCountedOrSummed(ProvableCountSumTree(_, 3, 100)) contributes
        // (0, 0) even though the inner aggregates are non-trivial.
        let inner = Element::new_provable_count_sum_tree_with_flags_and_sum_and_count_value(
            None, 3, 100, None,
        );
        let wrapped = Element::new_not_counted_or_summed(inner).expect("wrap ok");
        db.insert(
            [TEST_LEAF, b"outer"].as_ref(),
            b"w",
            wrapped,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert wrapped provable count sum tree");

        // Aggregate must be (count=1, sum=0): only `plain` contributes the
        // count; the wrapped tree's count=3 and sum=100 are both suppressed.
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
        assert_eq!(aggregate.as_count_u64(), 1);
        assert_eq!(aggregate.as_sum_i64(), 0);
    }

    #[test]
    fn batch_propagation_preserves_not_counted_or_summed_wrapper_on_subtree() {
        // A batch that inserts a NotCountedOrSummed(SumTree) AND writes a
        // child under it forces propagation through InsertTreeWithRootHash.
        // The on-disk parent element must come back wrapped, and the outer
        // tree's count and sum must both exclude the subtree.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        make_count_sum_tree_parent(&db, b"outer", grove_version);

        // Plain sum item contributes (1, 5).
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

        // Batch: insert NotCountedOrSummed(SumTree) under outer + a sum-item
        // child under that wrapped tree, forcing the propagation path.
        let wrapped =
            Element::new_not_counted_or_summed(Element::new_sum_tree(None)).expect("wrap ok");
        let inner_child = Element::new_sum_item(123);
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"outer".to_vec()],
                b"w".to_vec(),
                wrapped,
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"outer".to_vec(), b"w".to_vec()],
                b"child".to_vec(),
                inner_child,
            ),
        ];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch should succeed");

        // The element stored at outer/w must STILL be NotCountedOrSummed
        // after propagation. If it were dropped, the parent's count would
        // be 2 (instead of 1) and sum 128 (instead of 5).
        let stored = db
            .get_raw(
                grovedb_path::SubtreePath::from(&[TEST_LEAF, b"outer"]),
                b"w",
                None,
                grove_version,
            )
            .unwrap()
            .expect("get_raw w");
        assert!(
            matches!(stored, Element::NotCountedOrSummed(_)),
            "wrapper must survive batch propagation; got {:?}",
            stored
        );

        // Outer aggregate must NOT include the wrapped subtree's count (1)
        // or sum (123). Only `plain` contributes (1, 5).
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
            "wrapped subtree must not contribute to outer count; got {:?}",
            aggregate
        );
        assert_eq!(
            aggregate.as_sum_i64(),
            5,
            "wrapped subtree must not contribute to outer sum; got {:?}",
            aggregate
        );
    }

    #[test]
    fn batch_propagation_preserves_wrapper_under_provable_count_sum_tree() {
        // Same as the CountSumTree propagation test but with a
        // ProvableCountSumTree parent — exercises the
        // ProvableCountSumTree propagation arm in batch_structure.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        make_provable_count_sum_tree_parent(&db, b"outer", grove_version);

        // Plain item contributes (1, 0).
        db.insert(
            [TEST_LEAF, b"outer"].as_ref(),
            b"plain",
            Element::new_item(b"x".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert plain");

        let wrapped =
            Element::new_not_counted_or_summed(Element::new_provable_count_sum_tree(None))
                .expect("wrap ok");
        let inner_child = Element::new_sum_item(99);
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"outer".to_vec()],
                b"w".to_vec(),
                wrapped,
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"outer".to_vec(), b"w".to_vec()],
                b"child".to_vec(),
                inner_child,
            ),
        ];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch should succeed");

        // Wrapper must survive propagation.
        let stored = db
            .get_raw(
                grovedb_path::SubtreePath::from(&[TEST_LEAF, b"outer"]),
                b"w",
                None,
                grove_version,
            )
            .unwrap()
            .expect("get_raw w");
        assert!(
            matches!(stored, Element::NotCountedOrSummed(_)),
            "wrapper must survive batch propagation; got {:?}",
            stored
        );

        // Outer aggregate: count=1 (plain only), sum=0.
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
        let aggregate = outer_merk.aggregate_data().expect("read aggregate");
        assert_eq!(aggregate.as_count_u64(), 1);
        assert_eq!(aggregate.as_sum_i64(), 0);
    }

    #[test]
    fn check_subtree_exists_through_not_counted_or_summed_wrapper() {
        // A NotCountedOrSummed-wrapped tree at the parent path must satisfy
        // check_subtree_exists, otherwise APIs that gate on it would reject
        // paths through wrapped parents.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        make_count_sum_tree_parent(&db, b"outer", grove_version);

        // NotCountedOrSummed(SumTree) inside the outer count-sum tree.
        db.insert(
            [TEST_LEAF, b"outer"].as_ref(),
            b"w",
            Element::new_not_counted_or_summed(Element::new_sum_tree(None)).expect("wrap ok"),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert wrapped sum tree");

        // Inserting into the wrapped subtree exercises check_subtree_exists
        // on a path whose parent is `NotCountedOrSummed(SumTree)` — should
        // succeed.
        db.insert(
            [TEST_LEAF, b"outer", b"w"].as_ref(),
            b"child",
            Element::new_sum_item(42),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert into wrapped subtree must succeed");
    }

    #[test]
    fn constructor_invariants() {
        // Only the four sum-bearing tree variants are accepted as inner.
        assert!(Element::new_not_counted_or_summed(Element::new_sum_tree(None)).is_ok());
        assert!(Element::new_not_counted_or_summed(Element::new_big_sum_tree(None)).is_ok());
        assert!(Element::new_not_counted_or_summed(Element::new_count_sum_tree(None)).is_ok());
        assert!(
            Element::new_not_counted_or_summed(Element::new_provable_count_sum_tree(None)).is_ok()
        );

        // Non-sum-tree inners are rejected.
        assert!(Element::new_not_counted_or_summed(Element::new_item(b"x".to_vec())).is_err());
        assert!(Element::new_not_counted_or_summed(Element::new_sum_item(7)).is_err());
        assert!(Element::new_not_counted_or_summed(Element::new_tree(None)).is_err());
        assert!(Element::new_not_counted_or_summed(Element::new_count_tree(None)).is_err());
        assert!(
            Element::new_not_counted_or_summed(Element::new_provable_count_tree(None)).is_err()
        );

        // Wrappers cannot nest in any direction.
        let nc = Element::new_non_counted(Element::new_sum_tree(None)).expect("nc ok");
        assert!(Element::new_not_counted_or_summed(nc).is_err());
        let ns = Element::new_not_summed(Element::new_sum_tree(None)).expect("ns ok");
        assert!(Element::new_not_counted_or_summed(ns).is_err());
        let ncos =
            Element::new_not_counted_or_summed(Element::new_sum_tree(None)).expect("ncos ok");
        assert!(Element::new_not_counted_or_summed(ncos.clone()).is_err());

        // And the other wrappers reject NotCountedOrSummed-as-inner.
        assert!(Element::new_non_counted(ncos.clone()).is_err());
        assert!(Element::new_not_summed(ncos).is_err());
    }
}
