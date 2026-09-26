//! Tests for how delete op generation reads the operations already pending in
//! the batch it builds into.
//!
//! The costs pinned here are those the builders charged before they borrowed
//! the pending operations: reading them differently must not change what is
//! built or what it costs, on the released version or the latest. The one
//! version-gated difference is a pending `DeleteTree` with
//! `SubelementsDeletionBehavior::Skip`, which counts as removing its child
//! before `GROVE_V4` and not from it.

#[cfg(test)]
mod tests {
    use std::{
        cell::{Cell, RefCell},
        collections::HashMap,
    };

    use grovedb_costs::OperationCost;
    use grovedb_merk::{tree_type::TreeType, MaybeTree};
    use grovedb_path::SubtreePath;
    use grovedb_version::version::{v3::GROVE_V3, GroveVersion};

    use crate::{
        batch::{GroveOp, QualifiedGroveDbOp, SubelementsDeletionBehavior},
        operations::delete::{DeleteOptions, DeleteUpTreeOptions, PendingOperations},
        tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
        Element, Error,
    };

    /// The latest released version and the latest version.
    fn versions() -> [&'static GroveVersion; 2] {
        [&GROVE_V3, GroveVersion::latest()]
    }

    /// An operation type of a caller building a batch, holding grove
    /// operations among its own. Like Drive's, it holds them inline.
    #[allow(clippy::large_enum_variant)]
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

    /// A caller's index of its pending grove operations by path, recording
    /// the paths it is asked about.
    struct PathIndex<'a> {
        by_path: HashMap<Vec<Vec<u8>>, Vec<&'a QualifiedGroveDbOp>>,
        asked: RefCell<Vec<Vec<Vec<u8>>>>,
    }

    impl<'a> PathIndex<'a> {
        fn new(operations: &'a [CallerOperation]) -> Self {
            let mut by_path: HashMap<Vec<Vec<u8>>, Vec<&'a QualifiedGroveDbOp>> = HashMap::new();
            for op in grove_operations(operations) {
                by_path.entry(op.path.to_path()).or_default().push(op);
            }
            Self {
                by_path,
                asked: RefCell::default(),
            }
        }

        fn asked(&self) -> Vec<Vec<Vec<u8>>> {
            self.asked.borrow().clone()
        }
    }

    impl<'a> PendingOperations<'a> for &PathIndex<'a> {
        fn at_path(&self, path: &[Vec<u8>]) -> impl Iterator<Item = &'a QualifiedGroveDbOp> {
            self.asked.borrow_mut().push(path.to_vec());
            self.by_path.get(path).into_iter().flatten().copied()
        }
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

    /// The cost of building deletes, which only read.
    fn read_cost(
        seek_count: u32,
        storage_loaded_bytes: u64,
        hash_node_calls: u32,
    ) -> OperationCost {
        OperationCost {
            seek_count,
            storage_loaded_bytes,
            hash_node_calls,
            ..Default::default()
        }
    }

    fn insert_all(
        db: &TempGroveDb,
        inserts: Vec<(&[&[u8]], &[u8], Element)>,
        grove_version: &GroveVersion,
    ) {
        for (path_segments, key, element) in inserts {
            db.insert(path_segments, key, element, None, None, grove_version)
                .unwrap()
                .expect("should insert");
        }
    }

    /// `TEST_LEAF` holding `item`, the empty tree `empty`, `tree` with the
    /// items `x` and `y`, and `f` holding the tree `g` with the items `h` and
    /// `i`.
    fn setup(grove_version: &GroveVersion) -> TempGroveDb {
        let db = make_test_grovedb(grove_version);
        insert_all(
            &db,
            vec![
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
            ],
            grove_version,
        );
        db
    }

    /// Builds the delete of `key` under `path` against `pending` three ways —
    /// as a slice, borrowed through a caller's own operation type, and through
    /// an index of those by path — and returns the result and its cost after
    /// checking all three agree.
    fn build_all_ways(
        db: &TempGroveDb,
        path_segments: &[&[u8]],
        key: &[u8],
        pending: Vec<QualifiedGroveDbOp>,
        grove_version: &GroveVersion,
    ) -> (Result<Option<QualifiedGroveDbOp>, Error>, OperationCost) {
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
        let index = PathIndex::new(&caller_operations);
        let indexed = db.delete_operation_for_delete_internal(
            SubtreePath::from(path_segments),
            key,
            &DeleteOptions::default(),
            None,
            &index,
            None,
            grove_version,
        );
        for other in [&borrowed, &indexed] {
            assert_eq!(from_slice.cost, other.cost);
            assert_eq!(
                format!("{:?}", from_slice.value),
                format!("{:?}", other.value)
            );
        }
        (borrowed.value, borrowed.cost)
    }

    #[test]
    fn should_not_read_pending_operations_for_a_non_tree_delete() {
        for grove_version in versions() {
            let db = setup(grove_version);
            for is_known_to_be_subtree in [Some(MaybeTree::NotTree), None] {
                let read = Cell::new(false);
                let pending = std::iter::from_fn(|| {
                    read.set(true);
                    None::<&QualifiedGroveDbOp>
                });
                let built = db.delete_operation_for_delete_internal(
                    SubtreePath::from([TEST_LEAF].as_ref()),
                    b"item",
                    &DeleteOptions::default(),
                    is_known_to_be_subtree,
                    pending,
                    None,
                    grove_version,
                );
                let expected_cost = match is_known_to_be_subtree {
                    None => read_cost(2, 238, 1),
                    Some(_) => OperationCost::default(),
                };
                assert_eq!(built.cost, expected_cost);
                let op = built
                    .value
                    .expect("should build the delete")
                    .expect("should delete the item");
                assert!(matches!(op.op, GroveOp::Delete));
                assert!(!read.get(), "a non-tree delete read the pending operations");

                let caller_operations = [CallerOperation::Grove(delete(&[TEST_LEAF], b"empty"))];
                let index = PathIndex::new(&caller_operations);
                db.delete_operation_for_delete_internal(
                    SubtreePath::from([TEST_LEAF].as_ref()),
                    b"item",
                    &DeleteOptions::default(),
                    is_known_to_be_subtree,
                    &index,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("should build the delete");
                assert!(
                    index.asked().is_empty(),
                    "a non-tree delete asked the index"
                );
            }
        }
    }

    #[test]
    fn should_read_pending_operations_at_the_tree_path_for_a_tree_delete() {
        for grove_version in versions() {
            let db = setup(grove_version);

            let read = Cell::new(false);
            let pending = std::iter::from_fn(|| {
                read.set(true);
                None::<&QualifiedGroveDbOp>
            });
            let built = db.delete_operation_for_delete_internal(
                SubtreePath::from([TEST_LEAF].as_ref()),
                b"empty",
                &DeleteOptions::default(),
                None,
                pending,
                None,
                grove_version,
            );
            assert_eq!(built.cost, read_cost(5, 712, 3));
            let op = built
                .value
                .expect("should build the delete")
                .expect("should delete the empty tree");
            assert!(matches!(op.op, GroveOp::DeleteTree(..)));
            assert!(read.get(), "a tree delete skipped the pending operations");

            let caller_operations = [CallerOperation::Other];
            let index = PathIndex::new(&caller_operations);
            db.delete_operation_for_delete_internal(
                SubtreePath::from([TEST_LEAF].as_ref()),
                b"empty",
                &DeleteOptions::default(),
                None,
                &index,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should build the delete");
            assert_eq!(index.asked(), vec![path(&[TEST_LEAF, b"empty"])]);
        }
    }

    #[test]
    fn should_count_children_the_pending_batch_deletes() {
        for grove_version in versions() {
            let db = setup(grove_version);

            // Both children deleted earlier in the batch: the tree is empty.
            let (result, cost) = build_all_ways(
                &db,
                &[TEST_LEAF],
                b"tree",
                vec![
                    insert(&[TEST_LEAF, b"f"], b"elsewhere"),
                    delete(&[TEST_LEAF, b"tree"], b"x"),
                    delete(&[TEST_LEAF, b"tree"], b"y"),
                ],
                grove_version,
            );
            assert_eq!(cost, read_cost(8, 850, 3));
            let op = result
                .expect("should build the delete")
                .expect("should delete the emptied tree");
            assert!(matches!(
                op.op,
                GroveOp::DeleteTree(_, SubelementsDeletionBehavior::DontCheckWithNoCleanup)
            ));

            // One child deleted, or deletes at other paths: not empty.
            for (pending, expected_cost) in [
                (
                    vec![delete(&[TEST_LEAF, b"tree"], b"x")],
                    read_cost(7, 562, 3),
                ),
                (
                    vec![
                        delete(&[TEST_LEAF], b"item"),
                        delete(&[TEST_LEAF, b"f"], b"g"),
                    ],
                    read_cost(6, 529, 3),
                ),
            ] {
                let (result, cost) =
                    build_all_ways(&db, &[TEST_LEAF], b"tree", pending, grove_version);
                assert!(matches!(result, Err(Error::DeletingNonEmptyTree(_))));
                assert_eq!(cost, expected_cost);
            }
        }
    }

    #[test]
    fn should_count_a_pending_write_into_the_tree_as_non_empty() {
        for grove_version in versions() {
            let db = setup(grove_version);

            let (result, cost) = build_all_ways(
                &db,
                &[TEST_LEAF],
                b"empty",
                vec![insert(&[TEST_LEAF, b"empty"], b"new")],
                grove_version,
            );
            assert!(matches!(result, Err(Error::DeletingNonEmptyTree(_))));
            assert_eq!(cost, read_cost(5, 712, 3));
        }
    }

    #[test]
    fn should_count_a_pending_append_into_a_non_merk_tree_as_non_empty() {
        for grove_version in versions() {
            let db = make_test_grovedb(grove_version);
            insert_all(
                &db,
                vec![(&[TEST_LEAF], b"mmr", Element::empty_mmr_tree())],
                grove_version,
            );

            // An append names the tree as the last segment of its path.
            let (result, cost) = build_all_ways(
                &db,
                &[TEST_LEAF],
                b"mmr",
                vec![QualifiedGroveDbOp::mmr_tree_append_op(
                    path(&[TEST_LEAF, b"mmr"]),
                    b"value".to_vec(),
                )],
                grove_version,
            );
            assert!(matches!(result, Err(Error::DeletingNonEmptyTree(_))));
            assert_eq!(cost, read_cost(2, 153, 1));

            // A delete at its path neither empties nor fills a non-Merk tree.
            let (result, cost) = build_all_ways(
                &db,
                &[TEST_LEAF],
                b"mmr",
                vec![delete(&[TEST_LEAF, b"mmr"], b"k")],
                grove_version,
            );
            let op = result
                .expect("should build the delete")
                .expect("should delete the empty tree");
            assert!(matches!(op.op, GroveOp::DeleteTree(TreeType::MmrTree, _)));
            assert_eq!(cost, read_cost(2, 153, 1));
        }
    }

    #[test]
    fn should_count_a_pending_skip_delete_as_removing_its_child_only_before_v4() {
        for (grove_version, counts_skip) in [(&GROVE_V3, true), (GroveVersion::latest(), false)] {
            // `parent` holds only `child`, which is not empty.
            let db = make_test_grovedb(grove_version);
            insert_all(
                &db,
                vec![
                    (&[TEST_LEAF], b"parent", Element::empty_tree()),
                    (&[TEST_LEAF, b"parent"], b"child", Element::empty_tree()),
                    (
                        &[TEST_LEAF, b"parent", b"child"],
                        b"leaf",
                        Element::new_item(b"leaf".to_vec()),
                    ),
                ],
                grove_version,
            );
            let delete_child = |behavior| {
                QualifiedGroveDbOp::delete_tree_op(
                    path(&[TEST_LEAF, b"parent"]),
                    b"child".to_vec(),
                    TreeType::NormalTree,
                    behavior,
                )
            };

            // The batch applies an `Error` delete of `child` or fails, so it
            // counts as removing `child` on every version.
            let (result, cost) = build_all_ways(
                &db,
                &[TEST_LEAF],
                b"parent",
                vec![delete_child(SubelementsDeletionBehavior::Error)],
                grove_version,
            );
            assert!(matches!(result, Ok(Some(_))));
            assert_eq!(cost, read_cost(6, 639, 3));

            // The batch drops a `Skip` delete of the non-empty `child`.
            let (result, cost) = build_all_ways(
                &db,
                &[TEST_LEAF],
                b"parent",
                vec![delete_child(SubelementsDeletionBehavior::Skip)],
                grove_version,
            );
            if counts_skip {
                // Legacy: counted as removed, so `parent` is deleted without
                // cleanup over the `child` the batch keeps.
                let op = result
                    .expect("should build the delete")
                    .expect("should delete the parent");
                assert!(matches!(
                    op.op,
                    GroveOp::DeleteTree(_, SubelementsDeletionBehavior::DontCheckWithNoCleanup)
                ));
                assert_eq!(cost, read_cost(6, 639, 3));
            } else {
                // `child` still counts as present, as if nothing were pending.
                assert!(matches!(result, Err(Error::DeletingNonEmptyTree(_))));
                assert_eq!(cost, read_cost(5, 351, 3));
            }
        }
    }

    /// The up-tree chain deleting `h` under `f/g`, built from
    /// `caller_operations` borrowed, from a slice, through an index by path,
    /// and appended to a `Vec` by
    /// `add_delete_operations_for_delete_up_tree_while_empty`. Checks all four
    /// agree and returns the result, its cost, the `Vec` after the append, and
    /// the paths the index was asked about.
    #[allow(clippy::type_complexity)]
    fn build_chain_all_ways(
        db: &TempGroveDb,
        options: &DeleteUpTreeOptions,
        caller_operations: &[CallerOperation],
        grove_version: &GroveVersion,
    ) -> (
        Result<Vec<QualifiedGroveDbOp>, Error>,
        OperationCost,
        Vec<QualifiedGroveDbOp>,
        Vec<Vec<Vec<u8>>>,
    ) {
        let chain_path = || SubtreePath::from([TEST_LEAF, b"f", b"g"].as_ref());
        let owned: Vec<QualifiedGroveDbOp> = grove_operations(caller_operations).cloned().collect();

        let borrowed = db.delete_operations_for_delete_up_tree_while_empty(
            chain_path(),
            b"h",
            options,
            Some(MaybeTree::NotTree),
            grove_operations(caller_operations),
            None,
            grove_version,
        );
        let from_slice = db.delete_operations_for_delete_up_tree_while_empty(
            chain_path(),
            b"h",
            options,
            Some(MaybeTree::NotTree),
            &owned,
            None,
            grove_version,
        );
        let index = PathIndex::new(caller_operations);
        let indexed = db.delete_operations_for_delete_up_tree_while_empty(
            chain_path(),
            b"h",
            options,
            Some(MaybeTree::NotTree),
            &index,
            None,
            grove_version,
        );
        let mut extended = owned.clone();
        let from_vec = db.add_delete_operations_for_delete_up_tree_while_empty(
            chain_path(),
            b"h",
            options,
            Some(MaybeTree::NotTree),
            &mut extended,
            None,
            grove_version,
        );

        for other in [&from_slice, &indexed] {
            assert_eq!(borrowed.cost, other.cost);
            assert_eq!(
                format!("{:?}", borrowed.value),
                format!("{:?}", other.value)
            );
        }
        assert_eq!(borrowed.cost, from_vec.cost);
        assert_eq!(
            format!("{:?}", borrowed.value.as_ref().map(|ops| Some(ops.clone()))),
            format!("{:?}", from_vec.value)
        );
        (borrowed.value, borrowed.cost, extended, index.asked())
    }

    fn deleted_keys(ops: &[QualifiedGroveDbOp]) -> Vec<&[u8]> {
        ops.iter()
            .map(|op| op.key.as_ref().expect("keyed delete").as_slice())
            .collect()
    }

    #[test]
    fn should_climb_through_trees_the_pending_batch_empties() {
        for grove_version in versions() {
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

            let (result, cost, extended, asked) =
                build_chain_all_ways(&db, &options, &caller_operations, grove_version);
            let ops = result.expect("should build the chain");
            assert_eq!(cost, read_cost(14, 1462, 7));
            assert_eq!(deleted_keys(&ops), vec![b"h".as_slice(), b"g", b"f"]);

            // Only the trees the chain climbs through are asked about; the
            // item `h` is not.
            assert_eq!(
                asked,
                vec![path(&[TEST_LEAF, b"f", b"g"]), path(&[TEST_LEAF, b"f"])]
            );

            // The `&mut Vec` form appends the chain's deletes after the
            // caller's operations.
            let mut expected: Vec<QualifiedGroveDbOp> =
                grove_operations(&caller_operations).cloned().collect();
            expected.extend(ops);
            assert_eq!(extended, expected);
        }
    }

    #[test]
    fn should_stop_climbing_at_a_tree_the_pending_batch_writes_into() {
        for grove_version in versions() {
            let db = setup(grove_version);
            let options = DeleteUpTreeOptions {
                stop_path_height: Some(0),
                ..Default::default()
            };
            // `g` is emptied, but the batch writes into `f`, so `f` stays.
            let caller_operations = vec![
                CallerOperation::Grove(delete(&[TEST_LEAF, b"f", b"g"], b"i")),
                CallerOperation::Other,
                CallerOperation::Grove(insert(&[TEST_LEAF, b"f"], b"new")),
            ];

            let (result, cost, _, _) =
                build_chain_all_ways(&db, &options, &caller_operations, grove_version);
            let ops = result.expect("should build the chain");
            assert_eq!(cost, read_cost(14, 1462, 7));
            assert_eq!(deleted_keys(&ops), vec![b"h".as_slice(), b"g"]);
        }
    }

    #[test]
    fn should_leave_the_batch_unchanged_when_the_chain_fails() {
        for grove_version in versions() {
            let db = setup(grove_version);
            // No stop height: the chain climbs to `TEST_LEAF`, which as a root
            // leaf cannot be deleted, after building the deletes below it.
            let options = DeleteUpTreeOptions::default();
            let caller_operations = vec![CallerOperation::Grove(delete(
                &[TEST_LEAF, b"f", b"g"],
                b"i",
            ))];

            let (result, cost, extended, _) =
                build_chain_all_ways(&db, &options, &caller_operations, grove_version);
            assert!(matches!(result, Err(Error::InvalidPath(_))));
            assert_eq!(cost, read_cost(14, 1462, 7));
            let expected: Vec<QualifiedGroveDbOp> =
                grove_operations(&caller_operations).cloned().collect();
            assert_eq!(extended, expected);
        }
    }
}
