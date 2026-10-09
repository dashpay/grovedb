//! Tests for `GroveOp::Move` (issue #1014): a batch op that renames a key
//! within its parent and keeps the stored nodes of everything under it.
//!
//! The invariants under test:
//! - a moved tree comes out exactly as one built at the new key: the grove
//!   root hash after the move equals that of a grove that never held the
//!   old key, and `verify_grovedb` is clean;
//! - every storage prefix under the old key is emptied, for nested Merk
//!   trees, non-Merk trees and indexed secondaries alike;
//! - swaps and chains work in one batch, combined with other ops;
//! - aggregates (sum, count, provable count) are unchanged by a move;
//! - everything a move cannot carry is refused before anything commits;
//! - the move is billed as storage that stays where it is.

mod tests {
    use std::cell::Cell;

    use grovedb_costs::storage_cost::removal::StorageRemovedBytes::{
        BasicStorageRemoval, NoStorageRemoval,
    };
    use grovedb_merk::{element::costs::ElementCostExtensions, proofs::Query};
    use grovedb_path::SubtreePath;
    use grovedb_storage::{rocksdb_storage::RocksDbStorage, RawIterator, Storage, StorageContext};
    use grovedb_version::version::{v3::GROVE_V3, GroveVersion};

    use crate::{
        batch::{QualifiedGroveDbOp, SubelementsDeletionBehavior},
        reference_path::ReferencePathType,
        tests::{make_test_grovedb, TempGroveDb, ANOTHER_TEST_LEAF, TEST_LEAF},
        Element, Error, GroveDb, PathQuery,
    };

    fn v(bytes: &[u8]) -> Vec<u8> {
        bytes.to_vec()
    }

    fn path(segments: &[&[u8]]) -> Vec<Vec<u8>> {
        segments.iter().map(|s| s.to_vec()).collect()
    }

    fn assert_grove_verifies(db: &TempGroveDb, grove_version: &GroveVersion) {
        let issues = db
            .verify_grovedb(None, true, false, grove_version)
            .expect("verify_grovedb");
        assert!(issues.is_empty(), "verification issues: {issues:?}");
    }

    fn root_hash(db: &TempGroveDb, grove_version: &GroveVersion) -> [u8; 32] {
        db.root_hash(None, grove_version).unwrap().unwrap()
    }

    fn subtree_root_hash(
        db: &TempGroveDb,
        subtree: &[&[u8]],
        grove_version: &GroveVersion,
    ) -> [u8; 32] {
        let tx = db.start_transaction();
        let merk = db
            .open_transactional_merk_at_path(SubtreePath::from(subtree), &tx, None, grove_version)
            .unwrap()
            .expect("open merk");
        merk.root_hash().unwrap()
    }

    /// The node value hash of `key` in the Merk at `subtree`: it commits to
    /// the element and, for a tree, to everything below it.
    fn value_hash_at(
        db: &TempGroveDb,
        subtree: &[&[u8]],
        key: &[u8],
        grove_version: &GroveVersion,
    ) -> [u8; 32] {
        let tx = db.start_transaction();
        let merk = db
            .open_transactional_merk_at_path(SubtreePath::from(subtree), &tx, None, grove_version)
            .unwrap()
            .expect("open merk");
        merk.get_value_hash(
            key,
            true,
            Some(&Element::value_defined_cost_for_serialized_value),
            grove_version,
        )
        .unwrap()
        .expect("read")
        .expect("present")
    }

    /// Number of records physically present in the data namespace of
    /// `prefix` (committed state).
    fn records_in(db: &TempGroveDb, prefix: [u8; 32]) -> usize {
        let tx = db.db.start_transaction();
        let ctx = db
            .db
            .get_transactional_storage_context_by_subtree_prefix(prefix, None, &tx)
            .unwrap();
        let mut iter = ctx.raw_iter();
        iter.seek_to_first().unwrap();
        let mut count = 0;
        while iter.valid().unwrap() {
            count += 1;
            iter.next().unwrap();
        }
        count
    }

    fn prefix_of(subtree: &[&[u8]]) -> [u8; 32] {
        RocksDbStorage::build_prefix(subtree.into()).unwrap()
    }

    fn secondary_prefix_of(subtree: &[&[u8]]) -> [u8; 32] {
        RocksDbStorage::secondary_prefix_for(
            &prefix_of(subtree),
            grovedb_element::indexed::IndexAxis::Count.tag(),
        )
        .unwrap()
    }

    fn insert(
        db: &TempGroveDb,
        at: &[&[u8]],
        key: &[u8],
        element: Element,
        grove_version: &GroveVersion,
    ) {
        db.insert(at, key, element, None, None, grove_version)
            .unwrap()
            .unwrap_or_else(|e| panic!("insert {}: {e}", hex::encode(key)));
    }

    fn apply(
        db: &TempGroveDb,
        ops: Vec<QualifiedGroveDbOp>,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        db.apply_batch(ops, None, None, grove_version).unwrap()
    }

    fn move_op(at: &[&[u8]], key: &[u8], new_key: &[u8]) -> QualifiedGroveDbOp {
        QualifiedGroveDbOp::move_op(path(at), v(key), v(new_key))
    }

    /// Build, under `parent/key`, a tree holding a bit of everything a move
    /// has to carry: items, a sibling reference, nested Merk trees of every
    /// aggregate kind (one wrapped), an MMR tree and an indexed tree.
    fn populate(db: &TempGroveDb, parent: &[&[u8]], key: &[u8], grove_version: &GroveVersion) {
        let mut root: Vec<&[u8]> = parent.to_vec();
        root.push(key);
        insert(db, parent, key, Element::empty_tree(), grove_version);
        for i in 0..20u8 {
            insert(
                db,
                &root,
                &[b'i', i],
                Element::new_item(vec![i; 12]),
                grove_version,
            );
        }
        insert(
            db,
            &root,
            b"sibling_ref",
            Element::new_reference(ReferencePathType::SiblingReference(vec![b'i', 3])),
            grove_version,
        );

        let mut inner = root.clone();
        inner.push(b"inner");
        insert(db, &root, b"inner", Element::empty_tree(), grove_version);
        for i in 0..10u8 {
            insert(
                db,
                &inner,
                &[b'x', i],
                Element::new_item(vec![i; 40]),
                grove_version,
            );
        }

        let mut sums = inner.clone();
        sums.push(b"sums");
        insert(
            db,
            &inner,
            b"sums",
            Element::empty_sum_tree(),
            grove_version,
        );
        for i in 0..6u8 {
            insert(
                db,
                &sums,
                &[b's', i],
                Element::new_sum_item(i as i64 * 7 - 10),
                grove_version,
            );
        }

        let mut counts = root.clone();
        counts.push(b"counts");
        insert(
            db,
            &root,
            b"counts",
            Element::empty_count_tree(),
            grove_version,
        );
        for i in 0..5u8 {
            insert(
                db,
                &counts,
                &[b'c', i],
                Element::new_item(vec![i]),
                grove_version,
            );
        }
        insert(
            db,
            &counts,
            b"uncounted",
            Element::new_non_counted(Element::empty_tree()).expect("wrap"),
            grove_version,
        );
        let mut uncounted = counts.clone();
        uncounted.push(b"uncounted");
        insert(
            db,
            &uncounted,
            b"deep",
            Element::new_item(v(b"deep value")),
            grove_version,
        );

        insert(db, &root, b"mmr", Element::empty_mmr_tree(), grove_version);
        for i in 0..5u8 {
            db.mmr_tree_append(root.as_slice(), b"mmr", vec![i; 8], None, grove_version)
                .unwrap()
                .expect("mmr append");
        }

        let mut cidx = root.clone();
        cidx.push(b"cidx");
        insert(
            db,
            &root,
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            grove_version,
        );
        // Indexed primaries take writes through batches, which mirror them
        // into the secondary.
        let rows = (0..4u8)
            .map(|i| {
                QualifiedGroveDbOp::insert_or_replace_op(
                    cidx.iter().map(|s| s.to_vec()).collect(),
                    vec![b'r', i],
                    Element::new_item(vec![i; 3]),
                )
            })
            .collect();
        apply(db, rows, grove_version).expect("populate indexed tree");
    }

    /// The subtree paths `populate` creates under `parent/key`.
    fn populated_subtrees<'a>(parent: &[&'a [u8]], key: &'a [u8]) -> Vec<Vec<&'a [u8]>> {
        let mut root = parent.to_vec();
        root.push(key);
        let with = |suffix: &[&'a [u8]]| {
            let mut p = root.clone();
            p.extend_from_slice(suffix);
            p
        };
        vec![
            root.clone(),
            with(&[b"inner"]),
            with(&[b"inner", b"sums"]),
            with(&[b"counts"]),
            with(&[b"counts", b"uncounted"]),
            with(&[b"mmr"]),
            with(&[b"cidx"]),
        ]
    }

    // ── A move is a rename ──────────────────────────────────────────────

    #[test]
    fn moved_tree_is_identical_to_one_built_at_the_new_key() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        populate(&db, &[TEST_LEAF], b"old_key", grove_version);
        let subtree_hash_before = subtree_root_hash(&db, &[TEST_LEAF, b"old_key"], grove_version);

        apply(
            &db,
            vec![move_op(&[TEST_LEAF], b"old_key", b"new_key")],
            grove_version,
        )
        .expect("move");

        let expected = make_test_grovedb(grove_version);
        populate(&expected, &[TEST_LEAF], b"new_key", grove_version);
        assert_eq!(
            root_hash(&db, grove_version),
            root_hash(&expected, grove_version),
            "the grove must equal one that built the tree at the new key"
        );
        assert_eq!(
            subtree_root_hash(&db, &[TEST_LEAF, b"new_key"], grove_version),
            subtree_hash_before
        );
        assert_grove_verifies(&db, grove_version);

        // Nothing is left under the old key, in any namespace.
        assert!(matches!(
            db.get([TEST_LEAF].as_ref(), b"old_key", None, grove_version)
                .unwrap(),
            Err(Error::PathKeyNotFound(_))
        ));
        for subtree in populated_subtrees(&[TEST_LEAF], b"old_key") {
            assert_eq!(records_in(&db, prefix_of(&subtree)), 0, "{subtree:?}");
        }
        assert_eq!(
            records_in(&db, secondary_prefix_of(&[TEST_LEAF, b"old_key", b"cidx"])),
            0
        );
        // And under the new key every namespace holds what a grove built
        // there holds.
        for subtree in populated_subtrees(&[TEST_LEAF], b"new_key") {
            assert_eq!(
                records_in(&db, prefix_of(&subtree)),
                records_in(&expected, prefix_of(&subtree)),
                "{subtree:?}"
            );
        }
        let new_secondary = secondary_prefix_of(&[TEST_LEAF, b"new_key", b"cidx"]);
        assert!(records_in(&db, new_secondary) > 0);
        assert_eq!(
            records_in(&db, new_secondary),
            records_in(&expected, new_secondary)
        );
    }

    #[test]
    fn moved_contents_read_and_prove_at_the_new_key() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        populate(&db, &[TEST_LEAF], b"old_key", grove_version);

        apply(
            &db,
            vec![move_op(&[TEST_LEAF], b"old_key", b"new_key")],
            grove_version,
        )
        .expect("move");

        // A sibling reference inside the moved tree still reaches its target.
        assert_eq!(
            db.get(
                [TEST_LEAF, b"new_key"].as_ref(),
                b"sibling_ref",
                None,
                grove_version
            )
            .unwrap()
            .expect("follow sibling reference"),
            Element::new_item(vec![3; 12])
        );
        assert_eq!(
            db.get(
                [TEST_LEAF, b"new_key", b"counts", b"uncounted"].as_ref(),
                b"deep",
                None,
                grove_version
            )
            .unwrap()
            .expect("deep item"),
            Element::new_item(v(b"deep value"))
        );
        assert_eq!(
            db.mmr_tree_get_value(
                [TEST_LEAF, b"new_key"].as_ref(),
                b"mmr",
                2,
                None,
                grove_version
            )
            .unwrap()
            .expect("mmr value"),
            Some(vec![2; 8])
        );

        let mut query = Query::new();
        query.insert_all();
        let path_query = PathQuery::new_unsized(path(&[TEST_LEAF, b"new_key", b"inner"]), query);
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove");
        let (hash, results) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).expect("verify proof");
        assert_eq!(hash, root_hash(&db, grove_version));
        assert_eq!(results.len(), 11);
    }

    #[test]
    fn swap_in_one_batch() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        populate(&db, &[TEST_LEAF], b"a", grove_version);
        insert(
            &db,
            &[TEST_LEAF],
            b"b",
            Element::empty_sum_tree(),
            grove_version,
        );
        insert(
            &db,
            &[TEST_LEAF, b"b"],
            b"one",
            Element::new_sum_item(5),
            grove_version,
        );
        // A record key both subtrees share: each lands on the other's prefix.
        insert(
            &db,
            &[TEST_LEAF, b"b"],
            b"inner",
            Element::new_sum_item(9),
            grove_version,
        );
        let a_hash = subtree_root_hash(&db, &[TEST_LEAF, b"a"], grove_version);
        let b_hash = subtree_root_hash(&db, &[TEST_LEAF, b"b"], grove_version);
        let a_element = db
            .get_raw([TEST_LEAF].as_ref().into(), b"a", None, grove_version)
            .unwrap()
            .unwrap();
        let b_element = db
            .get_raw([TEST_LEAF].as_ref().into(), b"b", None, grove_version)
            .unwrap()
            .unwrap();

        apply(
            &db,
            vec![
                move_op(&[TEST_LEAF], b"a", b"b"),
                move_op(&[TEST_LEAF], b"b", b"a"),
            ],
            grove_version,
        )
        .expect("swap");

        assert_eq!(
            subtree_root_hash(&db, &[TEST_LEAF, b"b"], grove_version),
            a_hash
        );
        assert_eq!(
            subtree_root_hash(&db, &[TEST_LEAF, b"a"], grove_version),
            b_hash
        );
        assert_eq!(
            db.get_raw([TEST_LEAF].as_ref().into(), b"a", None, grove_version)
                .unwrap()
                .unwrap(),
            b_element
        );
        assert_eq!(
            db.get_raw([TEST_LEAF].as_ref().into(), b"b", None, grove_version)
                .unwrap()
                .unwrap(),
            a_element
        );
        assert_eq!(
            db.get([TEST_LEAF, b"a"].as_ref(), b"inner", None, grove_version)
                .unwrap()
                .expect("b's record at a"),
            Element::new_sum_item(9)
        );
        assert!(matches!(
            db.get([TEST_LEAF, b"b"].as_ref(), b"inner", None, grove_version)
                .unwrap()
                .expect("a's tree at b"),
            Element::Tree(..)
        ));
        assert_grove_verifies(&db, grove_version);
        // a's nested trees left a's prefixes; only b's two records are there.
        assert_eq!(records_in(&db, prefix_of(&[TEST_LEAF, b"a"])), 2);
        assert_eq!(records_in(&db, prefix_of(&[TEST_LEAF, b"a", b"inner"])), 0);
        assert!(records_in(&db, prefix_of(&[TEST_LEAF, b"b", b"inner"])) > 0);
    }

    #[test]
    fn chain_and_other_ops_in_one_batch() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        for (key, value) in [(b"a", 1u8), (b"b", 2)] {
            insert(&db, &[TEST_LEAF], key, Element::empty_tree(), grove_version);
            insert(
                &db,
                &[TEST_LEAF, key],
                b"k",
                Element::new_item(vec![value]),
                grove_version,
            );
        }

        apply(
            &db,
            vec![
                move_op(&[TEST_LEAF], b"a", b"b"),
                move_op(&[TEST_LEAF], b"b", b"c"),
                QualifiedGroveDbOp::insert_or_replace_op(
                    path(&[TEST_LEAF]),
                    v(b"d"),
                    Element::new_item(v(b"new")),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    path(&[ANOTHER_TEST_LEAF]),
                    v(b"e"),
                    Element::new_item(v(b"elsewhere")),
                ),
            ],
            grove_version,
        )
        .expect("chain");

        let item_at = |subtree: &[u8]| {
            db.get([TEST_LEAF, subtree].as_ref(), b"k", None, grove_version)
                .unwrap()
                .expect("item")
        };
        assert_eq!(item_at(b"b"), Element::new_item(vec![1]));
        assert_eq!(item_at(b"c"), Element::new_item(vec![2]));
        assert!(db
            .get([TEST_LEAF].as_ref(), b"a", None, grove_version)
            .unwrap()
            .is_err());
        assert_eq!(records_in(&db, prefix_of(&[TEST_LEAF, b"a"])), 0);
        assert_grove_verifies(&db, grove_version);
    }

    #[test]
    fn a_move_survives_backward_reference_planning_elsewhere() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        populate(&db, &[TEST_LEAF], b"old_key", grove_version);
        insert(
            &db,
            &[ANOTHER_TEST_LEAF],
            b"target",
            Element::new_item_allowing_bidirectional_references(v(b"t")),
            grove_version,
        );

        apply(
            &db,
            vec![
                move_op(&[TEST_LEAF], b"old_key", b"new_key"),
                QualifiedGroveDbOp::insert_or_replace_op(
                    path(&[ANOTHER_TEST_LEAF]),
                    v(b"bidi"),
                    Element::new_bidirectional_reference(ReferencePathType::SiblingReference(v(
                        b"target",
                    ))),
                ),
            ],
            grove_version,
        )
        .expect("a move next to a bidirectional reference insert");

        let expected = make_test_grovedb(grove_version);
        populate(&expected, &[TEST_LEAF], b"new_key", grove_version);
        assert_eq!(
            subtree_root_hash(&db, &[TEST_LEAF], grove_version),
            subtree_root_hash(&expected, &[TEST_LEAF], grove_version)
        );
        assert_eq!(
            db.get([ANOTHER_TEST_LEAF].as_ref(), b"bidi", None, grove_version)
                .unwrap()
                .expect("bidirectional reference resolves"),
            Element::new_item_allowing_bidirectional_references(v(b"t"))
        );
        assert_grove_verifies(&db, grove_version);
    }

    #[test]
    fn root_level_tree_moves() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        populate(&db, &[TEST_LEAF], b"t", grove_version);

        apply(
            &db,
            vec![move_op(&[], TEST_LEAF, b"renamed_leaf")],
            grove_version,
        )
        .expect("move a root-level tree");

        assert_grove_verifies(&db, grove_version);
        assert_eq!(
            db.get(
                [b"renamed_leaf".as_slice(), b"t"].as_ref(),
                b"sibling_ref",
                None,
                grove_version
            )
            .unwrap()
            .expect("reference under the renamed root tree"),
            Element::new_item(vec![3; 12])
        );
        for subtree in populated_subtrees(&[TEST_LEAF], b"t") {
            assert_eq!(records_in(&db, prefix_of(&subtree)), 0, "{subtree:?}");
        }
    }

    #[test]
    fn aggregates_are_unchanged_by_a_move() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert(
            &db,
            &[TEST_LEAF],
            b"sum",
            Element::empty_sum_tree(),
            grove_version,
        );
        insert(
            &db,
            &[TEST_LEAF, b"sum"],
            b"x",
            Element::new_sum_item(40),
            grove_version,
        );
        insert(
            &db,
            &[TEST_LEAF, b"sum"],
            b"y",
            Element::new_sum_item(2),
            grove_version,
        );
        insert(
            &db,
            &[TEST_LEAF, b"sum"],
            b"t",
            Element::empty_sum_tree(),
            grove_version,
        );
        insert(
            &db,
            &[TEST_LEAF, b"sum", b"t"],
            b"z",
            Element::new_sum_item(-7),
            grove_version,
        );
        insert(
            &db,
            &[TEST_LEAF],
            b"pct",
            Element::empty_provable_count_tree(),
            grove_version,
        );
        for i in 0..6u8 {
            insert(
                &db,
                &[TEST_LEAF, b"pct"],
                &[i],
                Element::new_item(vec![i]),
                grove_version,
            );
        }
        insert(
            &db,
            &[TEST_LEAF, b"pct"],
            b"sub",
            Element::empty_count_tree(),
            grove_version,
        );
        insert(
            &db,
            &[TEST_LEAF, b"pct", b"sub"],
            b"q",
            Element::new_item(v(b"q")),
            grove_version,
        );
        let sum_before = db
            .get_raw([TEST_LEAF].as_ref().into(), b"sum", None, grove_version)
            .unwrap()
            .unwrap();
        let pct_before = db
            .get_raw([TEST_LEAF].as_ref().into(), b"pct", None, grove_version)
            .unwrap()
            .unwrap();

        apply(
            &db,
            vec![
                move_op(&[TEST_LEAF, b"sum"], b"x", b"x2"),
                move_op(&[TEST_LEAF, b"sum"], b"t", b"t2"),
                move_op(&[TEST_LEAF, b"pct"], &[2], &[200]),
                move_op(&[TEST_LEAF, b"pct"], b"sub", b"sub2"),
            ],
            grove_version,
        )
        .expect("moves in aggregate trees");

        let sum_after = db
            .get_raw([TEST_LEAF].as_ref().into(), b"sum", None, grove_version)
            .unwrap()
            .unwrap();
        let pct_after = db
            .get_raw([TEST_LEAF].as_ref().into(), b"pct", None, grove_version)
            .unwrap()
            .unwrap();
        assert_eq!(sum_after.sum_value_or_default(), 35);
        assert_eq!(
            sum_after.sum_value_or_default(),
            sum_before.sum_value_or_default()
        );
        assert_eq!(
            pct_after.count_value_or_default(),
            pct_before.count_value_or_default()
        );
        assert_eq!(
            db.get(
                [TEST_LEAF, b"sum", b"t2"].as_ref(),
                b"z",
                None,
                grove_version
            )
            .unwrap()
            .expect("nested sum item"),
            Element::new_sum_item(-7)
        );
        assert_grove_verifies(&db, grove_version);
    }

    #[test]
    fn items_and_references_move_as_stored() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert(
            &db,
            &[TEST_LEAF],
            b"item",
            Element::new_item_with_flags(v(b"v"), Some(vec![1, 2])),
            grove_version,
        );
        insert(
            &db,
            &[ANOTHER_TEST_LEAF],
            b"target",
            Element::new_item(v(b"t")),
            grove_version,
        );
        insert(
            &db,
            &[TEST_LEAF],
            b"ref",
            Element::new_reference(ReferencePathType::AbsolutePathReference(path(&[
                ANOTHER_TEST_LEAF,
                b"target",
            ]))),
            grove_version,
        );

        apply(
            &db,
            vec![
                move_op(&[TEST_LEAF], b"item", b"item2"),
                move_op(&[TEST_LEAF], b"ref", b"ref2"),
            ],
            grove_version,
        )
        .expect("move non-tree elements");

        assert_eq!(
            db.get_raw([TEST_LEAF].as_ref().into(), b"item2", None, grove_version)
                .unwrap()
                .unwrap(),
            Element::new_item_with_flags(v(b"v"), Some(vec![1, 2]))
        );
        assert_eq!(
            db.get([TEST_LEAF].as_ref(), b"ref2", None, grove_version)
                .unwrap()
                .expect("moved reference resolves"),
            Element::new_item(v(b"t"))
        );
        assert_grove_verifies(&db, grove_version);
    }

    #[test]
    fn cousin_references_below_the_moved_element_keep_resolving() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert(
            &db,
            &[TEST_LEAF],
            b"tree",
            Element::empty_tree(),
            grove_version,
        );
        insert(
            &db,
            &[TEST_LEAF],
            b"other",
            Element::empty_tree(),
            grove_version,
        );
        insert(
            &db,
            &[TEST_LEAF, b"other"],
            b"x",
            Element::new_item(v(b"outside")),
            grove_version,
        );
        insert(
            &db,
            &[TEST_LEAF, b"tree"],
            b"s",
            Element::empty_tree(),
            grove_version,
        );
        insert(
            &db,
            &[TEST_LEAF, b"tree"],
            b"t",
            Element::empty_tree(),
            grove_version,
        );
        insert(
            &db,
            &[TEST_LEAF, b"tree", b"t"],
            b"x",
            Element::new_item(v(b"inside")),
            grove_version,
        );
        // `tree/x` swaps `tree` for `other`; `tree/s/x` swaps `s` for `t`.
        insert(
            &db,
            &[TEST_LEAF, b"tree"],
            b"x",
            Element::new_reference(ReferencePathType::CousinReference(v(b"other"))),
            grove_version,
        );
        insert(
            &db,
            &[TEST_LEAF, b"tree", b"s"],
            b"x",
            Element::new_reference(ReferencePathType::CousinReference(v(b"t"))),
            grove_version,
        );

        apply(
            &db,
            vec![move_op(&[TEST_LEAF], b"tree", b"moved")],
            grove_version,
        )
        .expect("move");

        assert_eq!(
            db.get([TEST_LEAF, b"moved"].as_ref(), b"x", None, grove_version)
                .unwrap()
                .expect("cousin outside"),
            Element::new_item(v(b"outside"))
        );
        assert_eq!(
            db.get(
                [TEST_LEAF, b"moved", b"s"].as_ref(),
                b"x",
                None,
                grove_version
            )
            .unwrap()
            .expect("cousin inside"),
            Element::new_item(v(b"inside"))
        );
        assert_grove_verifies(&db, grove_version);
    }

    #[test]
    fn references_into_the_old_key_dangle_like_after_a_delete() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert(
            &db,
            &[TEST_LEAF],
            b"tree",
            Element::empty_tree(),
            grove_version,
        );
        insert(
            &db,
            &[TEST_LEAF, b"tree"],
            b"x",
            Element::new_item(v(b"x")),
            grove_version,
        );
        insert(
            &db,
            &[ANOTHER_TEST_LEAF],
            b"abs",
            Element::new_reference(ReferencePathType::AbsolutePathReference(path(&[
                TEST_LEAF, b"tree", b"x",
            ]))),
            grove_version,
        );

        apply(
            &db,
            vec![move_op(&[TEST_LEAF], b"tree", b"moved")],
            grove_version,
        )
        .expect("move");

        assert!(db
            .get([ANOTHER_TEST_LEAF].as_ref(), b"abs", None, grove_version)
            .unwrap()
            .is_err());
    }

    #[test]
    fn move_without_batching_runs_as_a_batch_of_one() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        populate(&db, &[TEST_LEAF], b"old_key", grove_version);

        db.apply_operations_without_batching(
            vec![move_op(&[TEST_LEAF], b"old_key", b"new_key")],
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("move");

        let expected = make_test_grovedb(grove_version);
        populate(&expected, &[TEST_LEAF], b"new_key", grove_version);
        assert_eq!(
            root_hash(&db, grove_version),
            root_hash(&expected, grove_version)
        );
    }

    #[test]
    fn a_large_tree_moves() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let expected = make_test_grovedb(grove_version);
        for (grove, key) in [(&db, b"old".as_slice()), (&expected, b"new".as_slice())] {
            insert(
                grove,
                &[TEST_LEAF],
                key,
                Element::empty_tree(),
                grove_version,
            );
            let mut ops = Vec::new();
            for i in 0..40u32 {
                let sub = i.to_be_bytes().to_vec();
                ops.push(QualifiedGroveDbOp::insert_or_replace_op(
                    path(&[TEST_LEAF, key]),
                    sub.clone(),
                    Element::empty_tree(),
                ));
                for j in 0..25u32 {
                    ops.push(QualifiedGroveDbOp::insert_or_replace_op(
                        vec![TEST_LEAF.to_vec(), key.to_vec(), sub.clone()],
                        j.to_be_bytes().to_vec(),
                        Element::new_item(vec![(i + j) as u8; 30]),
                    ));
                }
            }
            apply(grove, ops, grove_version).expect("populate");
        }

        apply(
            &db,
            vec![move_op(&[TEST_LEAF], b"old", b"new")],
            grove_version,
        )
        .expect("move");

        assert_eq!(
            root_hash(&db, grove_version),
            root_hash(&expected, grove_version)
        );
        assert_eq!(records_in(&db, prefix_of(&[TEST_LEAF, b"old"])), 0);
        assert_eq!(
            records_in(&db, prefix_of(&[TEST_LEAF, b"old", &7u32.to_be_bytes()])),
            0
        );
    }

    #[test]
    fn non_merk_and_indexed_trees_move_with_their_data() {
        let grove_version = GroveVersion::latest();
        let build = |suffix: &[u8]| {
            let db = make_test_grovedb(grove_version);
            let key = |name: &[u8]| [name, suffix].concat();
            insert(
                &db,
                &[TEST_LEAF],
                &key(b"mmr"),
                Element::empty_mmr_tree(),
                grove_version,
            );
            insert(
                &db,
                &[TEST_LEAF],
                &key(b"bulk"),
                Element::empty_bulk_append_tree(2).expect("bulk"),
                grove_version,
            );
            insert(
                &db,
                &[TEST_LEAF],
                &key(b"dense"),
                Element::empty_dense_tree(4),
                grove_version,
            );
            insert(
                &db,
                &[TEST_LEAF],
                &key(b"cidx"),
                Element::empty_provable_count_indexed_tree(),
                grove_version,
            );
            for i in 0..9u8 {
                db.mmr_tree_append(
                    [TEST_LEAF].as_ref(),
                    &key(b"mmr"),
                    vec![i; 5],
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("mmr append");
                db.bulk_append(
                    [TEST_LEAF].as_ref(),
                    &key(b"bulk"),
                    vec![i; 6],
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("bulk append");
                db.dense_tree_insert(
                    [TEST_LEAF].as_ref(),
                    &key(b"dense"),
                    vec![i; 7],
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("dense insert");
            }
            let rows = (0..5u8)
                .map(|i| {
                    QualifiedGroveDbOp::insert_or_replace_op(
                        vec![TEST_LEAF.to_vec(), key(b"cidx")],
                        vec![i],
                        Element::new_item(vec![i; 4]),
                    )
                })
                .collect();
            apply(&db, rows, grove_version).expect("rows");
            db
        };
        let db = build(b"_old");
        let expected = build(b"_new");

        apply(
            &db,
            [b"mmr".as_slice(), b"bulk", b"dense", b"cidx"]
                .into_iter()
                .map(|name| {
                    move_op(
                        &[TEST_LEAF],
                        &[name, b"_old"].concat(),
                        &[name, b"_new"].concat(),
                    )
                })
                .collect(),
            grove_version,
        )
        .expect("moves");

        // Four moves in one Merk can leave it another shape than four
        // inserts, so compare what each node commits to rather than roots.
        assert_grove_verifies(&db, grove_version);
        for name in [b"mmr".as_slice(), b"bulk", b"dense", b"cidx"] {
            let old = [name, b"_old"].concat();
            let new = [name, b"_new"].concat();
            assert_eq!(
                value_hash_at(&db, &[TEST_LEAF], &new, grove_version),
                value_hash_at(&expected, &[TEST_LEAF], &new, grove_version),
                "{}",
                String::from_utf8_lossy(&new)
            );
            assert_eq!(records_in(&db, prefix_of(&[TEST_LEAF, &old])), 0);
            assert_eq!(
                records_in(&db, prefix_of(&[TEST_LEAF, &new])),
                records_in(&expected, prefix_of(&[TEST_LEAF, &new]))
            );
        }
        assert_eq!(
            records_in(&db, secondary_prefix_of(&[TEST_LEAF, b"cidx_old"])),
            0
        );
        assert_eq!(
            db.bulk_get_value([TEST_LEAF].as_ref(), b"bulk_new", 7, None, grove_version)
                .unwrap()
                .expect("bulk value"),
            Some(vec![7; 6])
        );
    }

    #[test]
    fn a_commitment_tree_moves_with_its_frontier() {
        use grovedb_commitment_tree::{DashMemo, NoteBytesData, TransmittedNoteCiphertext};

        let grove_version = GroveVersion::latest();
        let note = |index: u8| {
            let mut cmx = [0u8; 32];
            cmx[0] = index;
            let mut rho = [0u8; 32];
            rho[..2].copy_from_slice(&[index, 0xAA]);
            let mut cv_net = [0u8; 32];
            cv_net[..2].copy_from_slice(&[index, 0xCC]);
            let mut epk = [0u8; 32];
            epk[0] = index;
            let mut enc = [0u8; 104];
            enc[0] = index;
            let ciphertext = TransmittedNoteCiphertext::<DashMemo>::from_parts(
                epk,
                NoteBytesData(enc),
                [index; 80],
            );
            (cmx, rho, cv_net, ciphertext)
        };
        let build = |key: &[u8]| {
            let db = make_test_grovedb(grove_version);
            insert(
                &db,
                &[TEST_LEAF],
                key,
                Element::empty_commitment_tree(2).expect("chunk power"),
                grove_version,
            );
            for index in 1..=6u8 {
                let (cmx, rho, cv_net, ciphertext) = note(index);
                db.commitment_tree_insert(
                    [TEST_LEAF].as_ref(),
                    key,
                    cmx,
                    rho,
                    cv_net,
                    ciphertext,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("note");
            }
            db
        };
        let db = build(b"pool");
        let expected = build(b"pool2");

        apply(
            &db,
            vec![move_op(&[TEST_LEAF], b"pool", b"pool2")],
            grove_version,
        )
        .expect("move");

        assert_eq!(
            root_hash(&db, grove_version),
            root_hash(&expected, grove_version)
        );
        assert_grove_verifies(&db, grove_version);
        assert_eq!(
            db.commitment_tree_anchor([TEST_LEAF].as_ref(), b"pool2", None, grove_version)
                .unwrap()
                .expect("anchor at the new key"),
            expected
                .commitment_tree_anchor([TEST_LEAF].as_ref(), b"pool2", None, grove_version)
                .unwrap()
                .expect("anchor")
        );
        assert_eq!(records_in(&db, prefix_of(&[TEST_LEAF, b"pool"])), 0);
    }

    /// Platform's protocol version 14 rekeys unsigned integer index values
    /// by flipping their top bit (issue #1014): a key and its flipped twin
    /// trade places, and a key whose twin is absent lands on a free key.
    #[test]
    fn flipped_keys_trade_places_in_one_batch() {
        let grove_version = GroveVersion::latest();
        let values: Vec<u8> = vec![0, 1, 5, 100, 127, 128, 129, 150, 200, 228, 255];
        let build = |db: &TempGroveDb, key_of: &dyn Fn(u8) -> u8| {
            insert(
                db,
                &[TEST_LEAF],
                b"index",
                Element::empty_provable_count_tree(),
                grove_version,
            );
            for &value in &values {
                let key = [key_of(value)];
                insert(
                    db,
                    &[TEST_LEAF, b"index"],
                    &key,
                    Element::empty_count_tree(),
                    grove_version,
                );
                for document in 0..=value % 4 {
                    insert(
                        db,
                        &[TEST_LEAF, b"index", &key],
                        &[document],
                        Element::new_item(vec![value, document]),
                        grove_version,
                    );
                }
            }
        };
        let db = make_test_grovedb(grove_version);
        build(&db, &|value| value ^ 0x80);
        let expected = make_test_grovedb(grove_version);
        build(&expected, &|value| value);
        let count_before = db
            .get_raw([TEST_LEAF].as_ref().into(), b"index", None, grove_version)
            .unwrap()
            .unwrap()
            .count_value_or_default();

        apply(
            &db,
            values
                .iter()
                .map(|&value| move_op(&[TEST_LEAF, b"index"], &[value ^ 0x80], &[value]))
                .collect(),
            grove_version,
        )
        .expect("rekey");

        assert_grove_verifies(&db, grove_version);
        for &value in &values {
            assert_eq!(
                subtree_root_hash(&db, &[TEST_LEAF, b"index", &[value]], grove_version),
                subtree_root_hash(&expected, &[TEST_LEAF, b"index", &[value]], grove_version),
                "value {value}"
            );
            assert_eq!(
                db.get(
                    [TEST_LEAF, b"index", &[value]].as_ref(),
                    &[0],
                    None,
                    grove_version
                )
                .unwrap()
                .expect("document"),
                Element::new_item(vec![value, 0])
            );
        }
        assert_eq!(
            db.get_raw([TEST_LEAF].as_ref().into(), b"index", None, grove_version)
                .unwrap()
                .unwrap()
                .count_value_or_default(),
            count_before
        );
    }

    #[test]
    fn costs_are_not_estimated_for_moves() {
        let grove_version = GroveVersion::latest();
        let result = GroveDb::estimated_case_operations_for_batch(
            crate::batch::estimated_costs::EstimatedCostsType::AverageCaseCostsType(
                Default::default(),
            ),
            vec![move_op(&[TEST_LEAF], b"a", b"z")],
            None,
            |_, _, _| Ok(false),
            |_, key_bytes, value_bytes| {
                Ok((
                    BasicStorageRemoval(key_bytes),
                    BasicStorageRemoval(value_bytes),
                ))
            },
            grove_version,
        )
        .unwrap();
        assert!(
            matches!(result, Err(Error::NotSupported(_))),
            "got {result:?}"
        );
    }

    // ── Refusals ────────────────────────────────────────────────────────

    /// Apply `ops`, which must be refused with an error whose message
    /// contains `reason`, and leave the grove untouched.
    fn assert_refused(
        db: &TempGroveDb,
        ops: Vec<QualifiedGroveDbOp>,
        grove_version: &GroveVersion,
        reason: &str,
    ) {
        let before = root_hash(db, grove_version);
        match apply(db, ops, grove_version) {
            Err(error) => assert!(
                format!("{error:?}").contains(reason),
                "expected a refusal for \"{reason}\", got {error:?}"
            ),
            Ok(()) => panic!("expected a refusal for \"{reason}\""),
        }
        assert_eq!(
            root_hash(db, grove_version),
            before,
            "a refused batch must not commit"
        );
    }

    fn seeded(grove_version: &GroveVersion) -> TempGroveDb {
        let db = make_test_grovedb(grove_version);
        for key in [b"a".as_slice(), b"b"] {
            insert(&db, &[TEST_LEAF], key, Element::empty_tree(), grove_version);
            insert(
                &db,
                &[TEST_LEAF, key],
                b"k",
                Element::new_item(v(b"v")),
                grove_version,
            );
        }
        db
    }

    #[test]
    fn released_versions_refuse_moves() {
        let db = make_test_grovedb(&GROVE_V3);
        insert(&db, &[TEST_LEAF], b"a", Element::empty_tree(), &GROVE_V3);
        let result = apply(&db, vec![move_op(&[TEST_LEAF], b"a", b"z")], &GROVE_V3);
        assert!(
            matches!(result, Err(Error::VersionError(_))),
            "got {result:?}"
        );
    }

    #[test]
    fn malformed_moves_are_refused() {
        let grove_version = GroveVersion::latest();
        let db = seeded(grove_version);
        assert_refused(
            &db,
            vec![move_op(&[TEST_LEAF], b"a", b"a")],
            grove_version,
            "a new key different from its key",
        );
        assert_refused(
            &db,
            vec![move_op(&[TEST_LEAF], b"a", &[7; 256])],
            grove_version,
            "key length must be at most 255 bytes",
        );
        assert_refused(
            &db,
            vec![move_op(&[TEST_LEAF], b"missing", b"z")],
            grove_version,
            "it does not exist",
        );
        assert_refused(
            &db,
            vec![move_op(&[TEST_LEAF], b"a", b"b")],
            grove_version,
            "the target of a move already exists",
        );
        assert_refused(
            &db,
            vec![
                move_op(&[TEST_LEAF], b"a", b"z"),
                move_op(&[TEST_LEAF], b"b", b"z"),
            ],
            grove_version,
            "two moves in one batch have the same target",
        );
    }

    #[test]
    fn other_ops_on_moved_keys_are_refused() {
        let grove_version = GroveVersion::latest();
        let db = seeded(grove_version);
        let item = || Element::new_item(v(b"w"));
        let under = "cannot also change anything under its key or its target";
        let at_target = "can only be freed by moving its element away";
        // `(reason, op)`: each op is refused next to a move of `a` to `z`.
        let cases: Vec<(&str, QualifiedGroveDbOp)> = vec![
            (
                under,
                QualifiedGroveDbOp::insert_or_replace_op(path(&[TEST_LEAF, b"a"]), v(b"n"), item()),
            ),
            (
                under,
                QualifiedGroveDbOp::insert_or_replace_op(path(&[TEST_LEAF, b"z"]), v(b"n"), item()),
            ),
            (
                at_target,
                QualifiedGroveDbOp::insert_or_replace_op(path(&[TEST_LEAF]), v(b"z"), item()),
            ),
            (
                at_target,
                QualifiedGroveDbOp::delete_op(path(&[TEST_LEAF]), v(b"z")),
            ),
            (
                "cannot move an element inside a tree it deletes",
                QualifiedGroveDbOp::delete_tree_op(
                    path(&[]),
                    TEST_LEAF.to_vec(),
                    grovedb_merk::tree_type::TreeType::NormalTree,
                    SubelementsDeletionBehavior::DeleteChildren,
                ),
            ),
            (
                "references can not point to an element this batch moves",
                QualifiedGroveDbOp::insert_or_replace_op(
                    path(&[ANOTHER_TEST_LEAF]),
                    v(b"r"),
                    Element::new_reference(ReferencePathType::AbsolutePathReference(path(&[
                        TEST_LEAF, b"a", b"k",
                    ]))),
                ),
            ),
        ];
        for (reason, op) in cases {
            assert_refused(
                &db,
                vec![move_op(&[TEST_LEAF], b"a", b"z"), op],
                grove_version,
                reason,
            );
        }
        // A second op at the moved key is refused even with the consistency
        // check switched off.
        let result = db
            .apply_batch(
                vec![
                    move_op(&[TEST_LEAF], b"a", b"z"),
                    QualifiedGroveDbOp::delete_op(path(&[TEST_LEAF]), v(b"a")),
                ],
                Some(crate::batch::BatchApplyOptions {
                    disable_operation_consistency_check: true,
                    ..Default::default()
                }),
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            matches!(&result, Err(Error::InvalidBatchOperation(reason)) if reason.contains("cannot carry another operation")),
            "got {result:?}"
        );
    }

    #[test]
    fn unmovable_elements_are_refused() {
        let grove_version = GroveVersion::latest();
        let db = seeded(grove_version);
        // Inside an indexed tree.
        insert(
            &db,
            &[TEST_LEAF],
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            grove_version,
        );
        apply(
            &db,
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                path(&[TEST_LEAF, b"cidx"]),
                v(b"r"),
                Element::new_item(v(b"v")),
            )],
            grove_version,
        )
        .expect("indexed row");
        assert_refused(
            &db,
            vec![move_op(&[TEST_LEAF, b"cidx"], b"r", b"s")],
            grove_version,
            "a move inside an indexed tree is not supported",
        );
        // A cousin reference resolves through its own key.
        insert(
            &db,
            &[TEST_LEAF, b"b"],
            b"c",
            Element::new_item(v(b"c")),
            grove_version,
        );
        insert(
            &db,
            &[TEST_LEAF, b"a"],
            b"c",
            Element::new_reference(ReferencePathType::CousinReference(v(b"b"))),
            grove_version,
        );
        assert_refused(
            &db,
            vec![move_op(&[TEST_LEAF, b"a"], b"c", b"d")],
            grove_version,
            "resolves through its own key",
        );
        // A backward-reference participant, as the element or inside it.
        insert(
            &db,
            &[TEST_LEAF, b"b"],
            b"bidi",
            Element::new_item_allowing_bidirectional_references(v(b"x")),
            grove_version,
        );
        assert_refused(
            &db,
            vec![move_op(&[TEST_LEAF, b"b"], b"bidi", b"bidi2")],
            grove_version,
            "takes part in backward references",
        );
        assert_refused(
            &db,
            vec![move_op(&[TEST_LEAF], b"b", b"z")],
            grove_version,
            "takes part in backward references",
        );
    }

    #[test]
    fn partial_batches_refuse_moves() {
        let grove_version = GroveVersion::latest();
        let db = seeded(grove_version);
        let result = db
            .apply_partial_batch(
                vec![move_op(&[TEST_LEAF], b"a", b"z")],
                None,
                |_, _| Ok(vec![]),
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            matches!(result, Err(Error::NotSupported(_))),
            "got {result:?}"
        );
    }

    // ── Costs ───────────────────────────────────────────────────────────

    /// Apply `ops` through the flags-aware entry point, failing on any call
    /// of the flags-update callback and counting the removal callback's.
    fn apply_counting_callbacks(
        db: &TempGroveDb,
        ops: Vec<QualifiedGroveDbOp>,
        grove_version: &GroveVersion,
    ) -> (grovedb_costs::OperationCost, Vec<(u32, u32)>) {
        let removals = std::cell::RefCell::new(Vec::new());
        let flags_updates = Cell::new(0);
        let cost = db
            .apply_batch_with_element_flags_update(
                ops,
                None,
                |_, _, _| {
                    flags_updates.set(flags_updates.get() + 1);
                    Ok(false)
                },
                |_, key_bytes, value_bytes| {
                    removals.borrow_mut().push((key_bytes, value_bytes));
                    Ok((
                        BasicStorageRemoval(key_bytes),
                        BasicStorageRemoval(value_bytes),
                    ))
                },
                None,
                grove_version,
            )
            .cost_as_result()
            .expect("apply");
        assert_eq!(
            flags_updates.get(),
            0,
            "a move calls no flags update for what it carries, and these ancestors hold no flags"
        );
        (cost, removals.into_inner())
    }

    fn flagged_tree_with_contents(db: &TempGroveDb, key: &[u8], grove_version: &GroveVersion) {
        insert(
            db,
            &[TEST_LEAF],
            key,
            Element::empty_tree_with_flags(Some(vec![9, 9, 9])),
            grove_version,
        );
        for i in 0..8u8 {
            insert(
                db,
                &[TEST_LEAF, key],
                &[i],
                Element::new_item_with_flags(vec![i; 20], Some(vec![9, 9, 9])),
                grove_version,
            );
        }
    }

    #[test]
    fn a_same_length_move_adds_and_removes_nothing() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        flagged_tree_with_contents(&db, b"aaa", grove_version);

        let (cost, removals) = apply_counting_callbacks(
            &db,
            vec![move_op(&[TEST_LEAF], b"aaa", b"bbb")],
            grove_version,
        );

        assert_eq!(cost.storage_cost.added_bytes, 0, "{cost:?}");
        assert_eq!(
            cost.storage_cost.removed_bytes, NoStorageRemoval,
            "{cost:?}"
        );
        assert!(removals.is_empty(), "nothing is refunded: {removals:?}");
        assert!(cost.storage_cost.replaced_bytes > 0);
        assert!(cost.seek_count > 0 && cost.storage_loaded_bytes > 0);
        assert_grove_verifies(&db, grove_version);
    }

    #[test]
    fn a_longer_key_adds_and_a_shorter_one_removes_its_difference() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        flagged_tree_with_contents(&db, b"aaa", grove_version);

        let (same, _) = apply_counting_callbacks(
            &db,
            vec![move_op(&[TEST_LEAF], b"aaa", b"bbb")],
            grove_version,
        );
        let (longer, removals) = apply_counting_callbacks(
            &db,
            vec![move_op(&[TEST_LEAF], b"bbb", b"ccccc")],
            grove_version,
        );
        // Two more key bytes in the node's key and two more in its parent's
        // hook to it.
        assert_eq!(longer.storage_cost.added_bytes, 4, "{longer:?}");
        assert_eq!(longer.storage_cost.removed_bytes, NoStorageRemoval);
        assert!(removals.is_empty());

        let (shorter, removals) = apply_counting_callbacks(
            &db,
            vec![move_op(&[TEST_LEAF], b"ccccc", b"ddd")],
            grove_version,
        );
        assert_eq!(shorter.storage_cost.added_bytes, 0, "{shorter:?}");
        assert_eq!(shorter.storage_cost.removed_bytes.total_removed_bytes(), 4);
        assert_eq!(
            removals,
            vec![(2, 2)],
            "the freed bytes are sectioned through the callback"
        );
        // What is kept is billed the same whichever way the key changes.
        assert_eq!(
            same.storage_cost.replaced_bytes,
            shorter.storage_cost.replaced_bytes
        );
        assert_grove_verifies(&db, grove_version);
    }
}
