//! Tests for the two trees that keep a 128-bit sum next to
//! `BigSumTree` and `ProvableCountProvableBigSumTree`:
//!
//! - `CountBigSumTree`, the wide counterpart of `CountSumTree`: a count and
//!   an `i128` sum kept in the element, not in node hashes, so it accepts
//!   children that contribute nothing to the count, the sum or both.
//! - `ProvableBigSumTree`, the wide counterpart of `ProvableSumTree`: an
//!   `i128` sum in every node hash, no count, proved with its own nodes and
//!   summed on a range through the `i128` entry points.

#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::{query::QueryItem, Query};
    use grovedb_version::version::GroveVersion;

    use crate::{
        batch::QualifiedGroveDbOp,
        operations::proof::{GroveDBProof, ProofBytes},
        reference_path::ReferencePathType,
        tests::{make_test_grovedb, TempGroveDb},
        Element, Error, GroveDb, PathQuery, SizedQuery,
    };

    const TREE: &[u8] = b"tree";

    /// Three times `i64::MAX`: a total no `i64` aggregate can hold.
    const THREE_MAX: i128 = 3 * (i64::MAX as i128);

    fn insert(
        db: &TempGroveDb,
        path: &[&[u8]],
        key: &[u8],
        element: Element,
        grove_version: &GroveVersion,
    ) {
        db.insert(path, key, element, None, None, grove_version)
            .unwrap()
            .expect("should insert the element");
    }

    fn get(db: &TempGroveDb, path: &[&[u8]], key: &[u8], grove_version: &GroveVersion) -> Element {
        db.get(path, key, None, grove_version)
            .unwrap()
            .expect("should get the element")
    }

    fn root_hash(db: &TempGroveDb, grove_version: &GroveVersion) -> [u8; 32] {
        db.root_hash(None, grove_version)
            .unwrap()
            .expect("should compute the root hash")
    }

    /// The `(count, sum)` the `CountBigSumTree` under `key` at `path` records.
    fn count_big_sum(
        db: &TempGroveDb,
        path: &[&[u8]],
        key: &[u8],
        grove_version: &GroveVersion,
    ) -> (u64, i128) {
        get(db, path, key, grove_version)
            .as_count_big_sum_tree_value()
            .expect("should be a count big sum tree")
    }

    /// The sum the `ProvableBigSumTree` under `key` at `path` records.
    fn provable_big_sum(
        db: &TempGroveDb,
        path: &[&[u8]],
        key: &[u8],
        grove_version: &GroveVersion,
    ) -> i128 {
        get(db, path, key, grove_version)
            .as_provable_big_sum_tree_value()
            .expect("should be a provable big sum tree")
    }

    /// Prove `path_query` and verify it against the database root, returning
    /// the verified rows.
    fn prove_and_verify(
        db: &TempGroveDb,
        path_query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> Vec<(Vec<Vec<u8>>, Vec<u8>, Option<Element>)> {
        let proof = db
            .prove_query(path_query, None, grove_version)
            .unwrap()
            .expect("should prove the query");
        let (proven_root, rows) =
            GroveDb::verify_query(&proof, path_query, grove_version).expect("should verify");
        assert_eq!(
            proven_root,
            root_hash(db, grove_version),
            "the proof must verify against the database root"
        );
        rows
    }

    // ---------------------------------------------------------------------
    // CountBigSumTree
    // ---------------------------------------------------------------------

    /// Three children at `i64::MAX` are a total a `CountSumTree` refuses, and
    /// the wide tree holds it, in both directions, and gives it back on
    /// delete.
    #[test]
    fn count_big_sum_tree_holds_a_total_past_the_i64_range() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // The narrow tree refuses the second item.
        insert(
            &db,
            &[],
            b"narrow",
            Element::empty_count_sum_tree(),
            grove_version,
        );
        insert(
            &db,
            &[b"narrow"],
            b"a",
            Element::new_sum_item(i64::MAX),
            grove_version,
        );
        db.insert(
            &[b"narrow".as_slice()],
            b"b",
            Element::new_sum_item(1),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect_err("an i64 total cannot pass i64::MAX");

        insert(
            &db,
            &[],
            TREE,
            Element::empty_count_big_sum_tree(),
            grove_version,
        );
        for key in [b"a", b"b", b"c"] {
            insert(
                &db,
                &[TREE],
                key,
                Element::new_sum_item(i64::MAX),
                grove_version,
            );
        }
        assert_eq!(count_big_sum(&db, &[], TREE, grove_version), (3, THREE_MAX));

        for key in [b"d", b"e", b"f", b"g"] {
            insert(
                &db,
                &[TREE],
                key,
                Element::new_sum_item(i64::MIN),
                grove_version,
            );
        }
        assert_eq!(
            count_big_sum(&db, &[], TREE, grove_version),
            (7, THREE_MAX + 4 * (i64::MIN as i128))
        );

        db.delete(&[TREE], b"a", None, None, grove_version)
            .unwrap()
            .expect("should delete");
        assert_eq!(
            count_big_sum(&db, &[], TREE, grove_version),
            (6, 2 * (i64::MAX as i128) + 4 * (i64::MIN as i128))
        );
    }

    /// The property the provable wide tree lacks: a `CountBigSumTree` takes
    /// children wrapped to contribute nothing to its count, its sum or both.
    #[test]
    fn count_big_sum_tree_accepts_children_that_contribute_nothing() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert(
            &db,
            &[],
            TREE,
            Element::empty_count_big_sum_tree(),
            grove_version,
        );
        insert(
            &db,
            &[TREE],
            b"item",
            Element::new_sum_item(i64::MAX),
            grove_version,
        );
        assert_eq!(
            count_big_sum(&db, &[], TREE, grove_version),
            (1, i64::MAX as i128)
        );

        // Not counted: the sum of what is under it still counts.
        insert(
            &db,
            &[TREE],
            b"non_counted",
            Element::new_non_counted(Element::empty_sum_tree()).expect("a tree can be non counted"),
            grove_version,
        );
        insert(
            &db,
            &[TREE, b"non_counted"],
            b"x",
            Element::new_sum_item(i64::MAX),
            grove_version,
        );
        assert_eq!(
            count_big_sum(&db, &[], TREE, grove_version),
            (1, 2 * (i64::MAX as i128))
        );

        // Not summed: it counts, what is under it is not summed.
        insert(
            &db,
            &[TREE],
            b"not_summed",
            Element::new_not_summed(Element::empty_count_sum_tree())
                .expect("a sum-bearing tree can be not summed"),
            grove_version,
        );
        for key in [b"x", b"y"] {
            insert(
                &db,
                &[TREE, b"not_summed"],
                key,
                Element::new_sum_item(1_000),
                grove_version,
            );
        }
        assert_eq!(
            count_big_sum(&db, &[], TREE, grove_version),
            (3, 2 * (i64::MAX as i128))
        );

        // Not counted or summed: nothing of it reaches the parent.
        insert(
            &db,
            &[TREE],
            b"neither",
            Element::new_not_counted_or_summed(Element::empty_count_big_sum_tree())
                .expect("a sum-bearing tree can be not counted or summed"),
            grove_version,
        );
        for key in [b"x", b"y", b"z"] {
            insert(
                &db,
                &[TREE, b"neither"],
                key,
                Element::new_sum_item(i64::MAX),
                grove_version,
            );
        }
        assert_eq!(
            count_big_sum(&db, &[], TREE, grove_version),
            (3, 2 * (i64::MAX as i128))
        );
        // The wrapped tree keeps its own totals.
        assert_eq!(
            count_big_sum(&db, &[TREE], b"neither", grove_version),
            (3, THREE_MAX)
        );

        let issues = db
            .verify_grovedb(None, true, false, grove_version)
            .expect("should verify the database");
        assert!(issues.is_empty(), "unexpected issues: {issues:?}");
    }

    /// What a `CountBigSumTree` gives each kind of parent: its count and
    /// 128-bit sum to a parent that keeps both wide, its sum to a
    /// `BigSumTree`, and its count alone to a parent whose sum is an `i64`.
    #[test]
    fn count_big_sum_tree_contributes_by_the_width_of_its_parent() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        for (parent, element) in [
            (b"wide".as_slice(), Element::empty_count_big_sum_tree()),
            (
                b"provable_wide".as_slice(),
                Element::empty_provable_count_provable_big_sum_tree(),
            ),
            (b"big".as_slice(), Element::empty_big_sum_tree()),
            (b"narrow".as_slice(), Element::empty_count_sum_tree()),
        ] {
            insert(&db, &[], parent, element, grove_version);
            insert(
                &db,
                &[parent],
                b"child",
                Element::empty_count_big_sum_tree(),
                grove_version,
            );
            for key in [b"a", b"b", b"c"] {
                insert(
                    &db,
                    &[parent, b"child"],
                    key,
                    Element::new_sum_item(i64::MAX),
                    grove_version,
                );
            }
            assert_eq!(
                count_big_sum(&db, &[parent], b"child", grove_version),
                (3, THREE_MAX)
            );
        }

        assert_eq!(
            count_big_sum(&db, &[], b"wide", grove_version),
            (3, THREE_MAX)
        );
        assert_eq!(
            get(&db, &[], b"provable_wide", grove_version)
                .as_provable_count_provable_big_sum_tree_value()
                .expect("a wide tree"),
            (3, THREE_MAX)
        );
        assert_eq!(
            get(&db, &[], b"big", grove_version),
            Element::new_big_sum_tree_with_flags_and_sum_value(
                Some(b"child".to_vec()),
                THREE_MAX,
                None
            )
        );
        assert_eq!(
            get(&db, &[], b"narrow", grove_version),
            Element::new_count_sum_tree_with_flags_and_sum_and_count_value(
                Some(b"child".to_vec()),
                3,
                0,
                None
            )
        );

        let issues = db
            .verify_grovedb(None, true, false, grove_version)
            .expect("should verify the database");
        assert!(issues.is_empty(), "unexpected issues: {issues:?}");
    }

    /// A batch leaves the same tree, and the same root hash, as direct
    /// inserts.
    #[test]
    fn count_big_sum_tree_batch_matches_direct_inserts() {
        let grove_version = GroveVersion::latest();
        let direct = make_test_grovedb(grove_version);
        insert(
            &direct,
            &[],
            TREE,
            Element::empty_count_big_sum_tree(),
            grove_version,
        );
        for key in [b"a", b"b", b"c"] {
            insert(
                &direct,
                &[TREE],
                key,
                Element::new_sum_item(i64::MAX),
                grove_version,
            );
        }

        let batched = make_test_grovedb(grove_version);
        let mut ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![],
            TREE.to_vec(),
            Element::empty_count_big_sum_tree(),
        )];
        for key in [b"a", b"b", b"c"] {
            ops.push(QualifiedGroveDbOp::insert_or_replace_op(
                vec![TREE.to_vec()],
                key.to_vec(),
                Element::new_sum_item(i64::MAX),
            ));
        }
        batched
            .apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("should apply the batch");

        assert_eq!(
            count_big_sum(&batched, &[], TREE, grove_version),
            (3, THREE_MAX)
        );
        assert_eq!(
            root_hash(&batched, grove_version),
            root_hash(&direct, grove_version)
        );
    }

    /// Items of a `CountBigSumTree` are proved against the root like the
    /// items of any tree that commits no aggregate: its node hashes do not
    /// depend on the count or the sum.
    #[test]
    fn count_big_sum_tree_items_prove_against_the_root() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert(
            &db,
            &[],
            TREE,
            Element::empty_count_big_sum_tree(),
            grove_version,
        );
        for index in 0u8..20 {
            insert(
                &db,
                &[TREE],
                &[index],
                Element::new_sum_item(i64::MAX - index as i64),
                grove_version,
            );
        }

        let mut all = Query::new();
        all.insert_all();
        let rows = prove_and_verify(
            &db,
            &PathQuery::new_unsized(vec![TREE.to_vec()], all),
            grove_version,
        );
        assert_eq!(rows.len(), 20);

        let mut range = Query::new();
        range.insert_item(QueryItem::Range(vec![3]..vec![9]));
        let rows = prove_and_verify(
            &db,
            &PathQuery::new_unsized(vec![TREE.to_vec()], range),
            grove_version,
        );
        assert_eq!(rows.len(), 6);

        // The tree element itself, with the totals it records.
        let mut tree_row = Query::new();
        tree_row.insert_key(TREE.to_vec());
        let rows = prove_and_verify(
            &db,
            &PathQuery::new_unsized(vec![], tree_row),
            grove_version,
        );
        assert_eq!(rows.len(), 1);
        let recorded = rows[0].2.as_ref().expect("the tree row");
        assert_eq!(
            recorded
                .as_count_big_sum_tree_value()
                .expect("a count big sum tree"),
            (20, 20 * (i64::MAX as i128) - 190)
        );
    }

    /// A range sum needs the sum in the node hashes, which this tree does not
    /// keep there, so both widths of the query refuse it.
    #[test]
    fn count_big_sum_tree_refuses_range_aggregates() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert(
            &db,
            &[],
            TREE,
            Element::empty_count_big_sum_tree(),
            grove_version,
        );
        for index in 0u8..4 {
            insert(
                &db,
                &[TREE],
                &[index],
                Element::new_sum_item(1),
                grove_version,
            );
        }
        let range = QueryItem::Range(vec![0]..vec![3]);
        for query in [
            Query::new_aggregate_sum_on_range(range.clone()),
            Query::new_aggregate_count_on_range(range.clone()),
            Query::new_aggregate_count_and_sum_on_range(range),
        ] {
            let path_query = PathQuery::new_unsized(vec![TREE.to_vec()], query);
            assert!(
                db.prove_query(&path_query, None, grove_version)
                    .unwrap()
                    .is_err(),
                "a range aggregate must be refused on a CountBigSumTree"
            );
        }
    }

    // ---------------------------------------------------------------------
    // ProvableBigSumTree
    // ---------------------------------------------------------------------

    /// A database whose root holds a `ProvableBigSumTree` with the sum items
    /// `0..count`, the item at `index` holding `i64::MAX - index`.
    fn db_with_provable_big_sum_items(count: u8, grove_version: &GroveVersion) -> TempGroveDb {
        let db = make_test_grovedb(grove_version);
        insert(
            &db,
            &[],
            TREE,
            Element::empty_provable_big_sum_tree(),
            grove_version,
        );
        for index in 0..count {
            insert(
                &db,
                &[TREE],
                &[index],
                Element::new_sum_item(i64::MAX - index as i64),
                grove_version,
            );
        }
        db
    }

    /// The sum of the items `keys` of [`db_with_provable_big_sum_items`].
    fn sum_of(keys: std::ops::Range<u8>) -> i128 {
        keys.map(|index| (i64::MAX - index as i64) as i128).sum()
    }

    /// Three children at `i64::MAX` are a total a `ProvableSumTree` refuses,
    /// and the wide tree holds it.
    #[test]
    fn provable_big_sum_tree_holds_a_total_past_the_i64_range() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        insert(
            &db,
            &[],
            b"narrow",
            Element::empty_provable_sum_tree(),
            grove_version,
        );
        insert(
            &db,
            &[b"narrow"],
            b"a",
            Element::new_sum_item(i64::MAX),
            grove_version,
        );
        db.insert(
            &[b"narrow".as_slice()],
            b"b",
            Element::new_sum_item(1),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect_err("an i64 total cannot pass i64::MAX");

        insert(
            &db,
            &[],
            TREE,
            Element::empty_provable_big_sum_tree(),
            grove_version,
        );
        for key in [b"a", b"b", b"c"] {
            insert(
                &db,
                &[TREE],
                key,
                Element::new_sum_item(i64::MAX),
                grove_version,
            );
        }
        assert_eq!(provable_big_sum(&db, &[], TREE, grove_version), THREE_MAX);

        for key in [b"d", b"e", b"f", b"g"] {
            insert(
                &db,
                &[TREE],
                key,
                Element::new_sum_item(i64::MIN),
                grove_version,
            );
        }
        assert_eq!(
            provable_big_sum(&db, &[], TREE, grove_version),
            THREE_MAX + 4 * (i64::MIN as i128)
        );

        db.delete(&[TREE], b"a", None, None, grove_version)
            .unwrap()
            .expect("should delete");
        assert_eq!(
            provable_big_sum(&db, &[], TREE, grove_version),
            2 * (i64::MAX as i128) + 4 * (i64::MIN as i128)
        );

        let issues = db
            .verify_grovedb(None, true, false, grove_version)
            .expect("should verify the database");
        assert!(issues.is_empty(), "unexpected issues: {issues:?}");
    }

    /// The sum is part of every node hash, under a hash of its own: the same
    /// items give a different root under a `ProvableBigSumTree`, a
    /// `ProvableSumTree` and a `BigSumTree`.
    #[test]
    fn provable_big_sum_tree_root_hash_differs_from_its_neighbours() {
        let grove_version = GroveVersion::latest();
        let root_with = |tree: Element| {
            let db = make_test_grovedb(grove_version);
            insert(&db, &[], TREE, tree, grove_version);
            for index in 0u8..5 {
                insert(
                    &db,
                    &[TREE],
                    &[index],
                    Element::new_sum_item(index as i64 + 1),
                    grove_version,
                );
            }
            root_hash(&db, grove_version)
        };
        let wide = root_with(Element::empty_provable_big_sum_tree());
        assert_ne!(wide, root_with(Element::empty_provable_sum_tree()));
        assert_ne!(wide, root_with(Element::empty_big_sum_tree()));
        assert_ne!(
            wide,
            root_with(Element::empty_provable_count_provable_big_sum_tree())
        );
    }

    /// What the tree gives each kind of parent: its 128-bit sum to a wide
    /// parent, and nothing but a count of one to a parent whose sum is an
    /// `i64`.
    #[test]
    fn provable_big_sum_tree_contributes_by_the_width_of_its_parent() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        for (parent, element) in [
            (b"wide".as_slice(), Element::empty_provable_big_sum_tree()),
            (
                b"count_wide".as_slice(),
                Element::empty_count_big_sum_tree(),
            ),
            (b"narrow".as_slice(), Element::empty_count_sum_tree()),
        ] {
            insert(&db, &[], parent, element, grove_version);
            insert(
                &db,
                &[parent],
                b"child",
                Element::empty_provable_big_sum_tree(),
                grove_version,
            );
            for key in [b"a", b"b", b"c"] {
                insert(
                    &db,
                    &[parent, b"child"],
                    key,
                    Element::new_sum_item(i64::MAX),
                    grove_version,
                );
            }
        }
        assert_eq!(
            provable_big_sum(&db, &[], b"wide", grove_version),
            THREE_MAX
        );
        assert_eq!(
            count_big_sum(&db, &[], b"count_wide", grove_version),
            (1, THREE_MAX)
        );
        assert_eq!(
            get(&db, &[], b"narrow", grove_version),
            Element::new_count_sum_tree_with_flags_and_sum_and_count_value(
                Some(b"child".to_vec()),
                1,
                0,
                None
            )
        );

        let issues = db
            .verify_grovedb(None, true, false, grove_version)
            .expect("should verify the database");
        assert!(issues.is_empty(), "unexpected issues: {issues:?}");
    }

    /// The tree takes and refuses the same wrapped children as a
    /// `ProvableSumTree`.
    #[test]
    fn provable_big_sum_tree_takes_the_wrapped_children_a_provable_sum_tree_takes() {
        let grove_version = GroveVersion::latest();
        let outcome = |tree: Element, child: Element| {
            let db = make_test_grovedb(grove_version);
            insert(&db, &[], TREE, tree, grove_version);
            db.insert(&[TREE], b"child", child, None, None, grove_version)
                .unwrap()
                .is_ok()
        };
        for child in [
            Element::new_non_counted(Element::empty_sum_tree()).expect("non counted"),
            Element::new_not_summed(Element::empty_sum_tree()).expect("not summed"),
            Element::new_not_counted_or_summed(Element::empty_sum_tree())
                .expect("not counted or summed"),
            Element::empty_tree(),
            Element::new_item(vec![1]),
            Element::new_sum_item(7),
        ] {
            assert_eq!(
                outcome(Element::empty_provable_big_sum_tree(), child.clone()),
                outcome(Element::empty_provable_sum_tree(), child.clone()),
                "child {child}"
            );
        }
    }

    /// A batch leaves the same tree, and the same root hash, as direct
    /// inserts.
    #[test]
    fn provable_big_sum_tree_batch_matches_direct_inserts() {
        let grove_version = GroveVersion::latest();
        let direct = db_with_provable_big_sum_items(3, grove_version);

        let batched = make_test_grovedb(grove_version);
        let mut ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![],
            TREE.to_vec(),
            Element::empty_provable_big_sum_tree(),
        )];
        for index in 0u8..3 {
            ops.push(QualifiedGroveDbOp::insert_or_replace_op(
                vec![TREE.to_vec()],
                vec![index],
                Element::new_sum_item(i64::MAX - index as i64),
            ));
        }
        batched
            .apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("should apply the batch");

        assert_eq!(
            provable_big_sum(&batched, &[], TREE, grove_version),
            sum_of(0..3)
        );
        assert_eq!(
            root_hash(&batched, grove_version),
            root_hash(&direct, grove_version)
        );
    }

    /// Items are proved with the big sum bound into each node hash, over a
    /// full range, a limited range, both directions and absent keys.
    #[test]
    fn provable_big_sum_tree_items_prove_against_the_root() {
        let grove_version = GroveVersion::latest();
        let db = db_with_provable_big_sum_items(20, grove_version);

        let mut all = Query::new();
        all.insert_all();
        let rows = prove_and_verify(
            &db,
            &PathQuery::new_unsized(vec![TREE.to_vec()], all.clone()),
            grove_version,
        );
        assert_eq!(rows.len(), 20);
        for (index, (_, key, element)) in rows.iter().enumerate() {
            assert_eq!(key, &vec![index as u8]);
            assert_eq!(
                element,
                &Some(Element::new_sum_item(i64::MAX - index as i64))
            );
        }

        let limited = PathQuery::new(vec![TREE.to_vec()], SizedQuery::new(all, Some(5), None));
        assert_eq!(prove_and_verify(&db, &limited, grove_version).len(), 5);

        let mut descending = Query::new_with_direction(false);
        descending.insert_item(QueryItem::RangeFrom(vec![15]..));
        let rows = prove_and_verify(
            &db,
            &PathQuery::new_unsized(vec![TREE.to_vec()], descending),
            grove_version,
        );
        assert_eq!(
            rows.iter().map(|(_, key, _)| key[0]).collect::<Vec<_>>(),
            vec![19, 18, 17, 16, 15]
        );

        for absent in [20u8, 200] {
            let mut query = Query::new();
            query.insert_key(vec![absent]);
            let path_query =
                PathQuery::new(vec![TREE.to_vec()], SizedQuery::new(query, Some(1), None));
            let proof = db
                .prove_query(&path_query, None, grove_version)
                .unwrap()
                .expect("should prove the absence");
            let (proven_root, rows) =
                GroveDb::verify_query_with_absence_proof(&proof, &path_query, grove_version)
                    .expect("should verify the absence");
            assert_eq!(proven_root, root_hash(&db, grove_version));
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].2, None);
        }
    }

    /// A reference in the tree is proved with a node carrying the big sum,
    /// and the verified row is the value it points at.
    #[test]
    fn provable_big_sum_tree_reference_proves_against_the_root() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert(
            &db,
            &[],
            TREE,
            Element::empty_provable_big_sum_tree(),
            grove_version,
        );
        insert(&db, &[], b"values", Element::empty_tree(), grove_version);
        insert(
            &db,
            &[b"values"],
            b"target",
            Element::new_item(b"referenced".to_vec()),
            grove_version,
        );
        for key in [b"a", b"z"] {
            insert(
                &db,
                &[TREE],
                key,
                Element::new_sum_item(i64::MAX),
                grove_version,
            );
        }
        insert(
            &db,
            &[TREE],
            b"r",
            Element::new_reference_with_sum_item(
                ReferencePathType::AbsolutePathReference(vec![
                    b"values".to_vec(),
                    b"target".to_vec(),
                ]),
                i64::MAX,
            ),
            grove_version,
        );
        assert_eq!(provable_big_sum(&db, &[], TREE, grove_version), THREE_MAX);

        let mut query = Query::new();
        query.insert_key(b"r".to_vec());
        let rows = prove_and_verify(
            &db,
            &PathQuery::new_unsized(vec![TREE.to_vec()], query),
            grove_version,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].2, Some(Element::new_item(b"referenced".to_vec())));

        let mut all = Query::new();
        all.insert_all();
        let rows = prove_and_verify(
            &db,
            &PathQuery::new_unsized(vec![TREE.to_vec()], all),
            grove_version,
        );
        assert_eq!(rows.len(), 3);
    }

    /// A query descends through a `ProvableBigSumTree` nested in another:
    /// the tree row carries the big sum of the tree below it.
    #[test]
    fn provable_big_sum_tree_subquery_proves_against_the_root() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert(
            &db,
            &[],
            TREE,
            Element::empty_provable_big_sum_tree(),
            grove_version,
        );
        for group in [b"x", b"y"] {
            insert(
                &db,
                &[TREE],
                group,
                Element::empty_provable_big_sum_tree(),
                grove_version,
            );
            for key in [b"a", b"b", b"c"] {
                insert(
                    &db,
                    &[TREE, group],
                    key,
                    Element::new_sum_item(i64::MAX),
                    grove_version,
                );
            }
        }
        assert_eq!(
            provable_big_sum(&db, &[], TREE, grove_version),
            2 * THREE_MAX
        );

        let mut inner = Query::new();
        inner.insert_all();
        let mut outer = Query::new();
        outer.insert_all();
        outer.set_subquery(inner);
        let rows = prove_and_verify(
            &db,
            &PathQuery::new_unsized(vec![TREE.to_vec()], outer),
            grove_version,
        );
        assert_eq!(rows.len(), 6);
    }

    /// A proof whose big sum was altered no longer verifies: the sum is part
    /// of the node hash.
    #[test]
    fn provable_big_sum_tree_altered_sum_in_a_proof_is_refused() {
        use grovedb_merk::proofs::{encode_into, Decoder, Node, Op};

        let grove_version = GroveVersion::latest();
        let db = db_with_provable_big_sum_items(3, grove_version);

        let mut query = Query::new();
        query.insert_key(vec![1]);
        let path_query = PathQuery::new_unsized(vec![TREE.to_vec()], query);
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove the query");
        GroveDb::verify_query(&proof, &path_query, grove_version).expect("should verify as is");

        let config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        let altered_proof = |alter: &dyn Fn(i128) -> i128| -> Vec<u8> {
            let (mut decoded, _): (GroveDBProof, _) =
                bincode::decode_from_slice(&proof, config).expect("should decode the envelope");
            let GroveDBProof::V1(ref mut v1) = decoded else {
                panic!("the latest version proves with a V1 envelope");
            };
            let leaf = v1
                .root_layer
                .lower_layers
                .get_mut(TREE)
                .expect("the leaf layer is under the tree's key");
            let ProofBytes::Merk(ref mut bytes) = leaf.merk_proof else {
                panic!("a provable big sum tree is a Merk tree");
            };
            let mut seen = 0;
            let ops: Vec<Op> = Decoder::new(bytes)
                .map(|op| op.expect("should decode the op"))
                .map(|op| {
                    let rewrite = |node: Node, seen: &mut i32| match node {
                        Node::KVBigSum(key, value, sum) => {
                            *seen += 1;
                            Node::KVBigSum(key, value, alter(sum))
                        }
                        Node::KVHashBigSum(hash, sum) => {
                            *seen += 1;
                            Node::KVHashBigSum(hash, alter(sum))
                        }
                        other => other,
                    };
                    match op {
                        Op::Push(node) => Op::Push(rewrite(node, &mut seen)),
                        Op::PushInverted(node) => Op::PushInverted(rewrite(node, &mut seen)),
                        other => other,
                    }
                })
                .collect();
            assert!(seen > 0, "the leaf layer must hold big sum nodes");
            let mut rewritten = Vec::new();
            encode_into(ops.iter(), &mut rewritten);
            *bytes = rewritten;
            bincode::encode_to_vec(decoded, config).expect("should encode the envelope")
        };

        GroveDb::verify_query(&altered_proof(&|sum| sum), &path_query, grove_version)
            .expect("the re-encoded proof should verify");
        for alter in [
            (&|sum: i128| sum + 1) as &dyn Fn(i128) -> i128,
            &|sum: i128| sum + (1i128 << 64),
            &|sum: i128| -sum,
        ] {
            assert!(
                GroveDb::verify_query(&altered_proof(alter), &path_query, grove_version).is_err(),
                "an altered big sum must be refused"
            );
        }
    }

    /// A V0 envelope has no node for this tree, so proving through one is
    /// refused. The versions that prove with a V0 envelope cannot create the
    /// tree, so it is created under the latest one.
    #[test]
    fn provable_big_sum_tree_proof_is_refused_on_a_v0_envelope() {
        use grovedb_version::version::v2::GROVE_V2;
        let latest = GroveVersion::latest();

        let db = db_with_provable_big_sum_items(3, latest);
        let mut query = Query::new();
        query.insert_key(vec![1]);
        let path_query = PathQuery::new_unsized(vec![TREE.to_vec()], query);
        let err = db
            .prove_query(&path_query, None, &GROVE_V2)
            .unwrap()
            .expect_err("a V0 envelope must refuse the tree");
        assert!(
            matches!(err, Error::NotSupported(ref message) if message.contains("V1 proof envelopes")),
            "expected a V1 envelope refusal, got {err:?}"
        );
    }

    /// The sum on a range is proved against the root and read without a
    /// proof to the same value, as an `i128`. The `i64` entry points refuse
    /// the tree and its proofs, and the count queries refuse it: it keeps no
    /// count.
    #[test]
    fn provable_big_sum_tree_range_sums_prove_against_the_root() {
        let grove_version = GroveVersion::latest();
        let db = db_with_provable_big_sum_items(20, grove_version);
        let root = root_hash(&db, grove_version);

        for (range, expected) in [
            (QueryItem::Range(vec![2]..vec![10]), sum_of(2..10)),
            (QueryItem::RangeInclusive(vec![0]..=vec![19]), sum_of(0..20)),
            (QueryItem::RangeTo(..vec![5]), sum_of(0..5)),
            (QueryItem::RangeFrom(vec![17]..), sum_of(17..20)),
            (QueryItem::Range(vec![30]..vec![40]), 0),
        ] {
            let path_query = PathQuery::new_unsized(
                vec![TREE.to_vec()],
                Query::new_aggregate_sum_on_range(range.clone()),
            );
            let proof = db
                .prove_query(&path_query, None, grove_version)
                .unwrap()
                .expect("should prove the sum");
            assert_eq!(
                GroveDb::verify_aggregate_big_sum_query(&proof, &path_query, grove_version)
                    .expect("should verify the sum"),
                (root, expected),
                "range {range:?}"
            );
            assert!(
                GroveDb::verify_aggregate_sum_query(&proof, &path_query, grove_version).is_err(),
                "the i64 entry point must refuse a wide proof"
            );
            assert_eq!(
                db.query_aggregate_big_sum(&path_query, None, grove_version)
                    .unwrap()
                    .expect("should read the sum"),
                expected
            );
            assert!(
                db.query_aggregate_sum(&path_query, None, grove_version)
                    .unwrap()
                    .is_err(),
                "the i64 read must refuse a wide tree"
            );
        }

        let range = QueryItem::Range(vec![2]..vec![10]);
        for query in [
            Query::new_aggregate_count_on_range(range.clone()),
            Query::new_aggregate_count_and_sum_on_range(range),
        ] {
            let path_query = PathQuery::new_unsized(vec![TREE.to_vec()], query);
            assert!(
                db.prove_query(&path_query, None, grove_version)
                    .unwrap()
                    .is_err(),
                "a count on a range must be refused on a tree that keeps no count"
            );
        }
    }

    /// A range sum per outer key, each over a `ProvableBigSumTree`, one of
    /// them empty: proved against the root and read without a proof.
    #[test]
    fn provable_big_sum_tree_per_key_range_sums() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert(&db, &[], b"groups", Element::empty_tree(), grove_version);
        for (group, count) in [(b"w", 0u8), (b"x", 4), (b"y", 8), (b"z", 12)] {
            insert(
                &db,
                &[b"groups"],
                group,
                Element::empty_tree(),
                grove_version,
            );
            insert(
                &db,
                &[b"groups", group],
                b"values",
                Element::empty_provable_big_sum_tree(),
                grove_version,
            );
            for index in 0..count {
                insert(
                    &db,
                    &[b"groups", group, b"values"],
                    &[index],
                    Element::new_sum_item(i64::MAX - index as i64),
                    grove_version,
                );
            }
        }
        let mut carrier = Query::new();
        for group in [b"w", b"x", b"y", b"z"] {
            carrier.insert_key(group.to_vec());
        }
        carrier.set_subquery_path(vec![b"values".to_vec()]);
        carrier.set_subquery(Query::new_aggregate_sum_on_range(QueryItem::Range(
            vec![2]..vec![10],
        )));
        let path_query = PathQuery::new(
            vec![b"groups".to_vec()],
            SizedQuery::new(carrier, None, None),
        );
        let expected: Vec<(Vec<u8>, i128)> = vec![
            (b"w".to_vec(), 0),
            (b"x".to_vec(), sum_of(2..4)),
            (b"y".to_vec(), sum_of(2..8)),
            (b"z".to_vec(), sum_of(2..10)),
        ];

        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove the sums");
        assert_eq!(
            GroveDb::verify_aggregate_big_sum_query_per_key(&proof, &path_query, grove_version)
                .expect("should verify the sums"),
            (root_hash(&db, grove_version), expected.clone())
        );
        assert!(
            GroveDb::verify_aggregate_sum_query_per_key(&proof, &path_query, grove_version)
                .is_err()
        );
        assert_eq!(
            db.query_aggregate_big_sum_per_key(&path_query, None, grove_version)
                .unwrap()
                .expect("should read the sums"),
            expected
        );
    }

    /// Every byte of a proved range sum flipped in turn: no altered proof
    /// verifies to the root with another sum.
    #[test]
    fn provable_big_sum_tree_altered_range_sum_proof_is_refused() {
        let grove_version = GroveVersion::latest();
        let db = db_with_provable_big_sum_items(20, grove_version);
        let root = root_hash(&db, grove_version);
        let path_query = PathQuery::new_unsized(
            vec![TREE.to_vec()],
            Query::new_aggregate_sum_on_range(QueryItem::Range(vec![2]..vec![10])),
        );
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove the sum");
        let expected = sum_of(2..10);

        for position in 0..proof.len() {
            let mut altered = proof.clone();
            altered[position] ^= 0x01;
            if let Ok((proven_root, sum)) =
                GroveDb::verify_aggregate_big_sum_query(&altered, &path_query, grove_version)
            {
                assert!(
                    proven_root != root || sum == expected,
                    "byte {position} altered the sum to {sum} under the same root"
                );
            }
        }
    }

    // ---------------------------------------------------------------------
    // Both trees
    // ---------------------------------------------------------------------

    /// The released grove versions do not know the elements, so each refuses
    /// to create them, bare or wrapped, by a direct insert and by a batch.
    #[test]
    fn released_grove_versions_refuse_to_create_the_trees() {
        use grovedb_version::version::{v1::GROVE_V1, v2::GROVE_V2, v3::GROVE_V3, v4::GROVE_V4};

        let is_version_refusal = |result: Result<(), Error>| {
            matches!(
                result,
                Err(Error::VersionError(
                    grovedb_version::error::GroveVersionError::UnknownVersionMismatch { .. }
                ))
            )
        };

        for grove_version in [&GROVE_V1, &GROVE_V2, &GROVE_V3, &GROVE_V4] {
            for tree in [
                Element::empty_count_big_sum_tree(),
                Element::empty_provable_big_sum_tree(),
            ] {
                let db = make_test_grovedb(grove_version);
                insert(
                    &db,
                    &[],
                    b"count",
                    Element::empty_count_tree(),
                    grove_version,
                );

                let direct = db
                    .insert(
                        &[] as &[&[u8]],
                        TREE,
                        tree.clone(),
                        None,
                        None,
                        grove_version,
                    )
                    .unwrap();
                assert!(
                    is_version_refusal(direct),
                    "protocol version {} must refuse a direct insert of {tree}",
                    grove_version.protocol_version
                );

                let wrapped = db
                    .insert(
                        &[b"count".as_slice()],
                        TREE,
                        Element::new_non_counted(tree.clone()).expect("a tree can be non counted"),
                        None,
                        None,
                        grove_version,
                    )
                    .unwrap();
                assert!(
                    is_version_refusal(wrapped),
                    "protocol version {} must refuse a wrapped insert of {tree}",
                    grove_version.protocol_version
                );

                let batched = db
                    .apply_batch(
                        vec![QualifiedGroveDbOp::insert_or_replace_op(
                            vec![],
                            TREE.to_vec(),
                            tree.clone(),
                        )],
                        None,
                        None,
                        grove_version,
                    )
                    .unwrap();
                assert!(
                    is_version_refusal(batched),
                    "protocol version {} must refuse a batch insert of {tree}",
                    grove_version.protocol_version
                );
            }
        }
    }

    /// The estimates size both trees as the layered trees they are written
    /// as, at their own cost sizes.
    #[test]
    fn element_estimates_size_the_trees_as_layered() {
        use grovedb_merk::tree_type::TreeType;

        use crate::batch::key_info::KeyInfo::KnownKey;

        let grove_version = GroveVersion::latest();
        let key = KnownKey(b"tree".to_vec());
        for (element, tree_type) in [
            (
                Element::empty_count_big_sum_tree(),
                TreeType::CountBigSumTree,
            ),
            (
                Element::empty_provable_big_sum_tree(),
                TreeType::ProvableBigSumTree,
            ),
        ] {
            let average_element = GroveDb::average_case_merk_insert_element(
                &key,
                &element,
                TreeType::NormalTree,
                None,
                grove_version,
            );
            average_element.value.as_ref().expect("should estimate");
            let average_tree = GroveDb::average_case_merk_insert_tree(
                &key,
                &None,
                tree_type,
                TreeType::NormalTree,
                0,
                None,
                grove_version,
            );
            assert_eq!(average_element.cost, average_tree.cost, "{tree_type}");

            let worst_element = GroveDb::worst_case_merk_insert_element(
                &key,
                &element,
                TreeType::NormalTree,
                None,
                grove_version,
            );
            worst_element.value.as_ref().expect("should estimate");
            let worst_tree = GroveDb::worst_case_merk_insert_tree(
                &key,
                &None,
                tree_type,
                TreeType::NormalTree,
                0,
                None,
                grove_version,
            );
            assert_eq!(worst_element.cost, worst_tree.cost, "{tree_type}");

            let average_replace = GroveDb::average_case_merk_replace_element(
                &key,
                &element,
                TreeType::NormalTree,
                None,
                grove_version,
            );
            average_replace.value.as_ref().expect("should estimate");
            let worst_replace = GroveDb::worst_case_merk_replace_element(
                &key,
                &element,
                TreeType::NormalTree,
                None,
                grove_version,
            );
            worst_replace.value.as_ref().expect("should estimate");
            assert_eq!(average_replace.cost, worst_replace.cost, "{tree_type}");
        }
    }

    // ---------------------------------------------------------------------
    // Batches
    // ---------------------------------------------------------------------

    fn insert_op(path: &[&[u8]], key: &[u8], element: Element) -> QualifiedGroveDbOp {
        QualifiedGroveDbOp::insert_or_replace_op(
            path.iter().map(|segment| segment.to_vec()).collect(),
            key.to_vec(),
            element,
        )
    }

    /// A batch that creates a tree and writes into it replaces the insert
    /// with the tree's computed root before the insert is applied. The
    /// creation gate holds on that path too: a version that refuses the tree
    /// alone refuses it with contents, and writes nothing.
    #[test]
    fn released_grove_versions_refuse_a_tree_the_batch_also_fills() {
        use grovedb_version::version::v4::GROVE_V4;

        let v4 = &GROVE_V4;
        for tree in [
            Element::empty_count_big_sum_tree(),
            Element::empty_provable_big_sum_tree(),
            Element::empty_provable_count_provable_big_sum_tree(),
        ] {
            let db = make_test_grovedb(v4);
            insert(&db, &[], b"outer", Element::empty_tree(), v4);
            let before = root_hash(&db, v4);
            for ops in [
                vec![insert_op(&[b"outer"], TREE, tree.clone())],
                vec![
                    insert_op(&[b"outer"], TREE, tree.clone()),
                    insert_op(&[b"outer", TREE], b"k", Element::new_sum_item(5)),
                ],
                // Deeper still: the tree is an ancestor of what is written.
                vec![
                    insert_op(&[b"outer"], b"mid", Element::empty_tree()),
                    insert_op(&[b"outer", b"mid"], TREE, tree.clone()),
                    insert_op(&[b"outer", b"mid", TREE], b"k", Element::new_sum_item(5)),
                ],
            ] {
                match db.apply_batch(ops, None, None, v4).unwrap() {
                    Err(Error::VersionError(_)) => {}
                    other => panic!("expected a version error for {tree:?}, got {other:?}"),
                }
            }
            assert_eq!(root_hash(&db, v4), before);

            // The version that introduces the trees takes the same batch.
            let latest = GroveVersion::latest();
            let db = make_test_grovedb(latest);
            insert(&db, &[], b"outer", Element::empty_tree(), latest);
            db.apply_batch(
                vec![
                    insert_op(&[b"outer"], TREE, tree.clone()),
                    insert_op(&[b"outer", TREE], b"k", Element::new_sum_item(5)),
                ],
                None,
                None,
                latest,
            )
            .unwrap()
            .expect("create and fill at the latest version");
            let issues = db
                .verify_grovedb(None, true, true, latest)
                .expect("verify_grovedb");
            assert!(issues.is_empty(), "issues: {issues:?}");
        }
    }

    /// A count and big sum tree takes an indexed tree that is left out of
    /// its count, by a batch as by a direct insert.
    #[test]
    fn count_big_sum_tree_takes_a_non_counted_indexed_tree_in_a_batch() {
        let grove_version = GroveVersion::latest();
        let child = || Element::NonCounted(Box::new(Element::empty_provable_sum_indexed_tree()));

        let direct = make_test_grovedb(grove_version);
        insert(
            &direct,
            &[],
            TREE,
            Element::empty_count_big_sum_tree(),
            grove_version,
        );
        insert(&direct, &[TREE], b"w", child(), grove_version);

        let batched = make_test_grovedb(grove_version);
        insert(
            &batched,
            &[],
            TREE,
            Element::empty_count_big_sum_tree(),
            grove_version,
        );
        batched
            .apply_batch(
                vec![insert_op(&[TREE], b"w", child())],
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("batch insert");

        assert_eq!(
            root_hash(&batched, grove_version),
            root_hash(&direct, grove_version)
        );
        assert_eq!(
            count_big_sum(&batched, &[], TREE, grove_version),
            (0, 0),
            "the indexed tree is left out of the count"
        );
    }

    // ---------------------------------------------------------------------
    // Reads that return a value or an aggregate
    // ---------------------------------------------------------------------

    /// A read that returns each element's value or aggregate returns the
    /// count and the 128-bit sum of the trees that keep them, whether the
    /// tree is selected or a reference to it is.
    #[test]
    fn a_value_or_sum_read_returns_the_128_bit_aggregates() {
        use grovedb_version::version::v3::GROVE_V3;

        use crate::operations::get::QueryItemOrSumReturnType;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert(&db, &[], b"trees", Element::empty_tree(), grove_version);
        insert(&db, &[], b"refs", Element::empty_tree(), grove_version);
        for (key, tree) in [
            (b"a", Element::empty_count_big_sum_tree()),
            (b"b", Element::empty_provable_big_sum_tree()),
        ] {
            insert(&db, &[b"trees"], key, tree, grove_version);
            for item in 0u8..3 {
                insert(
                    &db,
                    &[b"trees", key],
                    &[item],
                    Element::new_sum_item(i64::MAX),
                    grove_version,
                );
            }
        }
        let expected = vec![
            QueryItemOrSumReturnType::CountBigSumValue(3, THREE_MAX),
            QueryItemOrSumReturnType::BigSumValue(THREE_MAX),
        ];
        let all_under = |tree: &[u8]| {
            let mut query = Query::new();
            query.insert_all();
            PathQuery::new_unsized(vec![tree.to_vec()], query)
        };

        let (results, _) = db
            .query_item_value_or_sum(&all_under(b"trees"), true, true, true, None, grove_version)
            .unwrap()
            .expect("read the trees");
        assert_eq!(results, expected);

        // A reference to a tree can only be legacy data: it is written under
        // GROVE_V3, which still accepts one, and read under the latest
        // version.
        for key in [b"a", b"b"] {
            db.insert(
                [b"refs".as_slice()].as_ref(),
                key,
                Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                    b"trees".to_vec(),
                    key.to_vec(),
                ])),
                None,
                None,
                &GROVE_V3,
            )
            .unwrap()
            .expect("insert a reference");
        }
        let (results, _) = db
            .query_item_value_or_sum(&all_under(b"refs"), true, true, true, None, grove_version)
            .unwrap()
            .expect("read through the references");
        assert_eq!(results, expected);
    }

    // ---------------------------------------------------------------------
    // As an entry of an indexed tree
    // ---------------------------------------------------------------------

    /// A count indexed tree takes a count and big sum tree as an entry and
    /// ranks it by the count its contents give it. Like every tree entry it
    /// enters empty: a tree without a root key that claims a count or a sum
    /// is refused, by the dedicated insert and by a batch.
    #[test]
    fn count_big_sum_tree_enters_an_indexed_tree_empty() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert(
            &db,
            &[],
            b"ranked",
            Element::empty_provable_count_indexed_tree(),
            grove_version,
        );
        let claims = || Element::CountBigSumTree(None, 3, 5, None);

        db.insert_into_count_indexed_tree(
            [b"ranked".as_slice()].as_ref(),
            b"a",
            Element::empty_count_big_sum_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("an empty entry by the dedicated insert");
        db.apply_batch(
            vec![insert_op(
                &[b"ranked"],
                b"b",
                Element::empty_count_big_sum_tree(),
            )],
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("an empty entry by a batch");

        let before = root_hash(&db, grove_version);
        db.insert_into_count_indexed_tree(
            [b"ranked".as_slice()].as_ref(),
            b"c",
            claims(),
            None,
            grove_version,
        )
        .unwrap()
        .expect_err("a claimed count and sum by the dedicated insert");
        db.apply_batch(
            vec![insert_op(&[b"ranked"], b"c", claims())],
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect_err("a claimed count and sum by a batch");
        assert_eq!(root_hash(&db, grove_version), before);

        // Filled, the entries give the indexed tree their counts.
        for (entry, items) in [(b"a", 2u8), (b"b", 1)] {
            for item in 0..items {
                insert(
                    &db,
                    &[b"ranked", entry],
                    &[item],
                    Element::new_sum_item(i64::MAX),
                    grove_version,
                );
            }
        }
        let Element::ProvableCountIndexedTree(_, _, count, _) =
            get(&db, &[], b"ranked", grove_version)
        else {
            panic!("expected a ProvableCountIndexedTree");
        };
        assert_eq!(count, 3);
        assert_eq!(
            count_big_sum(&db, &[b"ranked"], b"a", grove_version),
            (2, 2 * (i64::MAX as i128))
        );
        let issues = db
            .verify_grovedb(None, true, true, grove_version)
            .expect("verify_grovedb");
        assert!(issues.is_empty(), "issues: {issues:?}");
    }

    // ---------------------------------------------------------------------
    // Wrapped trees
    // ---------------------------------------------------------------------

    /// A wrapper is one more byte written. Both estimates count it for the
    /// trees with a 128-bit sum.
    #[test]
    fn element_estimates_count_the_wrapper_byte() {
        use grovedb_merk::tree_type::TreeType;
        use grovedb_version::version::v4::GROVE_V4;

        use crate::batch::key_info::KeyInfo::KnownKey;

        let grove_version = GroveVersion::latest();
        let key = KnownKey(b"tree".to_vec());
        let added_bytes = |element: &Element| {
            let average = GroveDb::average_case_merk_insert_element(
                &key,
                element,
                TreeType::CountSumTree,
                None,
                grove_version,
            );
            average.value.as_ref().expect("should estimate");
            let worst = GroveDb::worst_case_merk_insert_element(
                &key,
                element,
                TreeType::CountSumTree,
                None,
                grove_version,
            );
            worst.value.as_ref().expect("should estimate");
            (
                average.cost.storage_cost.added_bytes,
                worst.cost.storage_cost.added_bytes,
            )
        };
        for tree in [
            Element::empty_count_big_sum_tree(),
            Element::empty_provable_big_sum_tree(),
            Element::empty_provable_count_provable_big_sum_tree(),
        ] {
            let (average, worst) = added_bytes(&tree);
            for wrapped in [
                Element::NonCounted(Box::new(tree.clone())),
                Element::NotSummed(Box::new(tree.clone())),
                Element::NotCountedOrSummed(Box::new(tree.clone())),
            ] {
                assert_eq!(
                    added_bytes(&wrapped),
                    (average + 1, worst + 1),
                    "{wrapped:?}"
                );
            }
        }

        // A wrapped tree of a shipped type is counted too, under a parent
        // with a 128-bit sum and under a shipped one. `GROVE_V4` keeps the
        // average estimate it shipped with for a shipped pairing, without
        // the byte.
        let average_under = |parent: TreeType, element: &Element, version: &GroveVersion| {
            let estimate =
                GroveDb::average_case_merk_insert_element(&key, element, parent, None, version);
            estimate.value.as_ref().expect("should estimate");
            estimate.cost.storage_cost.added_bytes
        };
        let bare = Element::empty_sum_tree();
        let wrapped = Element::NotCountedOrSummed(Box::new(Element::empty_sum_tree()));
        for parent in [
            TreeType::CountBigSumTree,
            TreeType::ProvableBigSumTree,
            TreeType::ProvableCountProvableBigSumTree,
            TreeType::CountSumTree,
        ] {
            assert_eq!(
                average_under(parent, &wrapped, grove_version),
                average_under(parent, &bare, grove_version) + 1,
                "{parent}"
            );
        }
        assert_eq!(
            average_under(TreeType::CountSumTree, &wrapped, &GROVE_V4),
            average_under(TreeType::CountSumTree, &bare, &GROVE_V4),
        );
    }

    /// A wrapped tree that a batch creates and fills is held to its parent's
    /// rule for the wrapper when the tree or the parent has a 128-bit sum,
    /// as it is when the batch only creates it.
    #[test]
    fn a_filled_tree_is_held_to_the_wrapper_rule_of_its_parent() {
        let grove_version = GroveVersion::latest();
        let non_counted = |tree: Element| Element::NonCounted(Box::new(tree));
        let not_counted_or_summed = |tree: Element| Element::NotCountedOrSummed(Box::new(tree));
        // (parent, child, what the refusal says)
        let refused = [
            // A provable count commits its count and takes no child left
            // out of it.
            (
                Element::empty_provable_count_tree(),
                non_counted(Element::empty_count_big_sum_tree()),
                "non-counted elements may only be inserted",
            ),
            (
                Element::empty_provable_count_tree(),
                non_counted(Element::empty_provable_big_sum_tree()),
                "non-counted elements may only be inserted",
            ),
            // A provable big sum tree has no count to leave a child out of.
            (
                Element::empty_provable_big_sum_tree(),
                non_counted(Element::empty_sum_tree()),
                "non-counted elements may only be inserted",
            ),
            (
                Element::empty_provable_big_sum_tree(),
                not_counted_or_summed(Element::empty_sum_tree()),
                "not-counted-or-summed elements may only be inserted",
            ),
        ];
        for (parent, child, message) in refused {
            let db = make_test_grovedb(grove_version);
            insert(&db, &[], b"p", parent, grove_version);
            let before = root_hash(&db, grove_version);
            for ops in [
                vec![insert_op(&[b"p"], b"w", child.clone())],
                vec![
                    insert_op(&[b"p"], b"w", child.clone()),
                    insert_op(&[b"p", b"w"], b"k", Element::new_sum_item(5)),
                ],
            ] {
                let result = db.apply_batch(ops, None, None, grove_version).unwrap();
                let shown = format!("{result:?}");
                assert!(
                    result.is_err() && shown.contains(message),
                    "{child:?}: {shown}"
                );
            }
            assert_eq!(root_hash(&db, grove_version), before);
        }

        // Where the parent permits the wrapper, the filled tree is taken and
        // left out of what the wrapper names.
        let db = make_test_grovedb(grove_version);
        insert(
            &db,
            &[],
            b"p",
            Element::empty_count_big_sum_tree(),
            grove_version,
        );
        for (key, child) in [
            (b"a", non_counted(Element::empty_count_big_sum_tree())),
            (b"b", not_counted_or_summed(Element::empty_sum_tree())),
        ] {
            db.apply_batch(
                vec![
                    insert_op(&[b"p"], key, child),
                    insert_op(&[b"p", key], b"k", Element::new_sum_item(5)),
                ],
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("a wrapper the parent permits");
            // The batch wrote the tree's contents, not only the tree.
            assert_eq!(
                get(&db, &[b"p", key], b"k", grove_version),
                Element::new_sum_item(5)
            );
        }
        // The non-counted tree still adds its sum; the other adds nothing.
        assert_eq!(count_big_sum(&db, &[], b"p", grove_version), (0, 5));
        let issues = db
            .verify_grovedb(None, true, true, grove_version)
            .expect("verify_grovedb");
        assert!(issues.is_empty(), "issues: {issues:?}");
    }
}
