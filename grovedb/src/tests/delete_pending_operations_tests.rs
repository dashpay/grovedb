//! Tests for how delete op generation reads the operations already pending in
//! the batch it builds into.

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use grovedb_merk::{tree_type::TreeType, MaybeTree};
    use grovedb_path::SubtreePath;
    use grovedb_version::version::GroveVersion;

    use crate::{
        batch::{GroveOp, QualifiedGroveDbOp},
        operations::delete::{DeleteOptions, DeleteUpTreeOptions},
        tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
        Element, Error,
    };

    /// An operation type of a caller building a batch, holding grove
    /// operations among its own.
    enum CallerOperation {
        Grove(QualifiedGroveDbOp),
        Other,
    }

    /// The grove operations in `operations`, borrowed.
    fn grove_operations(
        operations: &[CallerOperation],
    ) -> impl Iterator<Item = &QualifiedGroveDbOp> + Clone {
        operations.iter().filter_map(|operation| match operation {
            CallerOperation::Grove(op) => Some(op),
            CallerOperation::Other => None,
        })
    }

    fn path(segments: &[&[u8]]) -> Vec<Vec<u8>> {
        segments.iter().map(|segment| segment.to_vec()).collect()
    }

    fn delete(path_segments: &[&[u8]], key: &[u8]) -> QualifiedGroveDbOp {
        QualifiedGroveDbOp::delete_op(path(path_segments), key.to_vec())
    }

    fn insert(path_segments: &[&[u8]], key: &[u8]) -> QualifiedGroveDbOp {
        QualifiedGroveDbOp::insert_or_replace_op(
            path(path_segments),
            key.to_vec(),
            Element::new_item(b"pending".to_vec()),
        )
    }

    /// `TEST_LEAF` holding `item`, the empty tree `empty`, `tree` with the
    /// items `x` and `y`, and `f` holding the tree `g` with the items `h` and
    /// `i`.
    fn setup(grove_version: &GroveVersion) -> TempGroveDb {
        let db = make_test_grovedb(grove_version);
        let inserts: [(&[&[u8]], &[u8], Element); 9] = [
            (&[TEST_LEAF], b"item", Element::new_item(b"item".to_vec())),
            (&[TEST_LEAF], b"empty", Element::empty_tree()),
            (&[TEST_LEAF], b"tree", Element::empty_tree()),
            (
                &[TEST_LEAF, b"tree"],
                b"x",
                Element::new_item(b"x".to_vec()),
            ),
            (
                &[TEST_LEAF, b"tree"],
                b"y",
                Element::new_item(b"y".to_vec()),
            ),
            (&[TEST_LEAF], b"f", Element::empty_tree()),
            (&[TEST_LEAF, b"f"], b"g", Element::empty_tree()),
            (
                &[TEST_LEAF, b"f", b"g"],
                b"h",
                Element::new_item(b"h".to_vec()),
            ),
            (
                &[TEST_LEAF, b"f", b"g"],
                b"i",
                Element::new_item(b"i".to_vec()),
            ),
        ];
        for (path_segments, key, element) in inserts {
            db.insert(path_segments, key, element, None, None, grove_version)
                .unwrap()
                .expect("should insert");
        }
        db
    }

    #[test]
    fn should_not_read_pending_operations_for_a_non_tree_delete() {
        let grove_version = GroveVersion::latest();
        let db = setup(grove_version);

        for is_known_to_be_subtree in [Some(MaybeTree::NotTree), None] {
            let read = Cell::new(false);
            let pending = std::iter::from_fn(|| {
                read.set(true);
                None::<&QualifiedGroveDbOp>
            });
            let op = db
                .delete_operation_for_delete_internal(
                    SubtreePath::from([TEST_LEAF].as_ref()),
                    b"item",
                    &DeleteOptions::default(),
                    is_known_to_be_subtree,
                    pending,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("should build the delete")
                .expect("should delete the item");
            assert!(matches!(op.op, GroveOp::Delete));
            assert!(!read.get(), "a non-tree delete read the pending operations");
        }
    }

    #[test]
    fn should_read_pending_operations_for_a_tree_delete() {
        let grove_version = GroveVersion::latest();
        let db = setup(grove_version);

        let read = Cell::new(false);
        let pending = std::iter::from_fn(|| {
            read.set(true);
            None::<&QualifiedGroveDbOp>
        });
        let op = db
            .delete_operation_for_delete_internal(
                SubtreePath::from([TEST_LEAF].as_ref()),
                b"empty",
                &DeleteOptions::default(),
                Some(MaybeTree::Tree(TreeType::NormalTree)),
                pending,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should build the delete")
            .expect("should delete the empty tree");
        assert!(matches!(op.op, GroveOp::DeleteTree(..)));
        assert!(read.get(), "a tree delete skipped the pending operations");
    }

    /// Builds the delete of `key` under `path` against `pending` once as a
    /// slice and once borrowed through a caller's own operation type, and
    /// returns the result after checking both agree, cost included.
    fn build_both_ways(
        db: &TempGroveDb,
        path_segments: &[&[u8]],
        key: &[u8],
        pending: Vec<QualifiedGroveDbOp>,
        grove_version: &GroveVersion,
    ) -> Result<Option<QualifiedGroveDbOp>, Error> {
        let from_slice = db.delete_operation_for_delete_internal(
            SubtreePath::from(path_segments),
            key,
            &DeleteOptions::default(),
            None,
            &pending,
            None,
            grove_version,
        );
        let caller_operations: Vec<CallerOperation> = pending
            .into_iter()
            .flat_map(|op| [CallerOperation::Other, CallerOperation::Grove(op)])
            .collect();
        let borrowed = db.delete_operation_for_delete_internal(
            SubtreePath::from(path_segments),
            key,
            &DeleteOptions::default(),
            None,
            grove_operations(&caller_operations),
            None,
            grove_version,
        );
        assert_eq!(from_slice.cost, borrowed.cost);
        assert_eq!(
            format!("{:?}", from_slice.value),
            format!("{:?}", borrowed.value)
        );
        borrowed.value
    }

    #[test]
    fn should_count_children_the_pending_batch_deletes() {
        let grove_version = GroveVersion::latest();
        let db = setup(grove_version);

        // Both children deleted earlier in the batch: the tree is empty.
        let op = build_both_ways(
            &db,
            &[TEST_LEAF],
            b"tree",
            vec![
                insert(&[TEST_LEAF, b"f"], b"elsewhere"),
                delete(&[TEST_LEAF, b"tree"], b"x"),
                delete(&[TEST_LEAF, b"tree"], b"y"),
            ],
            grove_version,
        )
        .expect("should build the delete")
        .expect("should delete the emptied tree");
        assert!(matches!(op.op, GroveOp::DeleteTree(..)));

        // One child deleted, or deletes at other paths: not empty.
        for pending in [
            vec![delete(&[TEST_LEAF, b"tree"], b"x")],
            vec![
                delete(&[TEST_LEAF], b"item"),
                delete(&[TEST_LEAF, b"f"], b"g"),
            ],
        ] {
            let result = build_both_ways(&db, &[TEST_LEAF], b"tree", pending, grove_version);
            assert!(matches!(result, Err(Error::DeletingNonEmptyTree(_))));
        }
    }

    #[test]
    fn should_count_a_pending_write_into_the_tree_as_non_empty() {
        let grove_version = GroveVersion::latest();
        let db = setup(grove_version);

        let result = build_both_ways(
            &db,
            &[TEST_LEAF],
            b"empty",
            vec![insert(&[TEST_LEAF, b"empty"], b"new")],
            grove_version,
        );
        assert!(matches!(result, Err(Error::DeletingNonEmptyTree(_))));
    }

    #[test]
    fn should_climb_through_trees_the_borrowed_pending_batch_empties() {
        let grove_version = GroveVersion::latest();
        let db = setup(grove_version);
        let options = DeleteUpTreeOptions {
            // Stop before the root leaf, which cannot be deleted.
            stop_path_height: Some(0),
            ..Default::default()
        };
        let caller_operations = vec![
            CallerOperation::Other,
            CallerOperation::Grove(insert(&[TEST_LEAF], b"elsewhere")),
            CallerOperation::Grove(delete(&[TEST_LEAF, b"f", b"g"], b"i")),
        ];
        let owned: Vec<QualifiedGroveDbOp> =
            grove_operations(&caller_operations).cloned().collect();

        let borrowed = db.delete_operations_for_delete_up_tree_while_empty(
            SubtreePath::from([TEST_LEAF, b"f", b"g"].as_ref()),
            b"h",
            &options,
            Some(MaybeTree::NotTree),
            grove_operations(&caller_operations),
            None,
            grove_version,
        );
        let from_slice = db.delete_operations_for_delete_up_tree_while_empty(
            SubtreePath::from([TEST_LEAF, b"f", b"g"].as_ref()),
            b"h",
            &options,
            Some(MaybeTree::NotTree),
            &owned,
            None,
            grove_version,
        );
        let mut extended = owned.clone();
        let from_vec = db.add_delete_operations_for_delete_up_tree_while_empty(
            SubtreePath::from([TEST_LEAF, b"f", b"g"].as_ref()),
            b"h",
            &options,
            Some(MaybeTree::NotTree),
            &mut extended,
            None,
            grove_version,
        );

        assert_eq!(borrowed.cost, from_slice.cost);
        assert_eq!(borrowed.cost, from_vec.cost);
        let ops = borrowed.value.expect("should build the chain");
        assert_eq!(ops, from_slice.value.expect("should build the chain"));
        assert_eq!(
            Some(ops.clone()),
            from_vec.value.expect("should build the chain")
        );
        let deleted_keys: Vec<&[u8]> = ops
            .iter()
            .map(|op| op.key.as_ref().expect("keyed delete").as_slice())
            .collect();
        assert_eq!(deleted_keys, vec![b"h".as_slice(), b"g", b"f"]);

        // The `&mut Vec` form still appends each level's delete for the
        // levels above it.
        let mut expected = owned;
        expected.extend(ops);
        assert_eq!(extended, expected);
    }
}
