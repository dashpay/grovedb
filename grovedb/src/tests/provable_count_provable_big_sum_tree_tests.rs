//! End-to-end behavior tests for `ProvableCountProvableBigSumTree` in GroveDB.
//!
//! `ProvableCountProvableBigSumTree` is the wide counterpart of
//! `ProvableCountProvableSumTree`: the element count and a 128-bit sum both
//! land in every node's hash, via `node_hash_with_count_and_big_sum`. Its
//! children contribute an `i64` each, so its total cannot overflow the way an
//! `i64` total can.
//!
//! Coverage:
//! 1. Count and sum tracking, including totals outside the `i64` range in
//!    both directions, through direct inserts, deletes and batches.
//! 2. What each kind of child contributes (sum items, items, narrow and wide
//!    sum trees, wrapped trees), and what the tree contributes to each kind
//!    of parent.
//! 3. Hash divergence from `ProvableCountProvableSumTree` over the same
//!    content.
//! 4. Proofs of items, references, absent keys and nested trees verifying
//!    against the GroveDB root, and their refusal on a V0 envelope.
//! 5. The range aggregates and count offsets that are not offered on this
//!    tree type are refused.
//! 6. `verify_grovedb` finds the tree consistent.

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

    const BIG: &[u8] = b"big";

    /// Three times `i64::MAX`: a total no `i64` aggregate can hold.
    const THREE_MAX: i128 = 3 * (i64::MAX as i128);

    /// Insert an empty `ProvableCountProvableBigSumTree` under `key` at
    /// `path`.
    fn insert_big_tree(db: &TempGroveDb, path: &[&[u8]], key: &[u8], grove_version: &GroveVersion) {
        db.insert(
            path,
            key,
            Element::empty_provable_count_provable_big_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert a provable count provable big sum tree");
    }

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

    /// The `(count, sum)` the tree element under `key` at `path` records.
    fn recorded(
        db: &TempGroveDb,
        path: &[&[u8]],
        key: &[u8],
        grove_version: &GroveVersion,
    ) -> (u64, i128) {
        db.get(path, key, None, grove_version)
            .unwrap()
            .expect("should get the tree")
            .as_provable_count_provable_big_sum_tree_value()
            .expect("should be a provable count provable big sum tree")
    }

    /// A database whose root holds a wide tree with the sum items `a`, `b`
    /// and `c`, each `i64::MAX`.
    fn db_with_three_max_items(grove_version: &GroveVersion) -> TempGroveDb {
        let db = make_test_grovedb(grove_version);
        insert_big_tree(&db, &[], BIG, grove_version);
        for key in [b"a", b"b", b"c"] {
            insert(
                &db,
                &[BIG],
                key,
                Element::new_sum_item(i64::MAX),
                grove_version,
            );
        }
        db
    }

    /// The headline: three children at `i64::MAX` are a total an `i64`
    /// aggregate refuses, and the wide tree holds it.
    #[test]
    fn big_sum_tree_holds_a_total_past_the_i64_range() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_big_tree(&db, &[], BIG, grove_version);

        let mut expected_sum: i128 = 0;
        for (index, key) in [b"a", b"b", b"c"].into_iter().enumerate() {
            insert(
                &db,
                &[BIG],
                key,
                Element::new_sum_item(i64::MAX),
                grove_version,
            );
            expected_sum += i64::MAX as i128;
            assert_eq!(
                recorded(&db, &[], BIG, grove_version),
                (index as u64 + 1, expected_sum)
            );
        }
        assert_eq!(recorded(&db, &[], BIG, grove_version), (3, THREE_MAX));
        assert!(THREE_MAX > i64::MAX as i128);
    }

    /// The same content in the narrow tree is refused at the second child,
    /// which is what the wide tree is for.
    #[test]
    fn narrow_tree_refuses_the_total_the_big_sum_tree_holds() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert(
            &db,
            &[],
            b"narrow",
            Element::empty_provable_count_provable_sum_tree(),
            grove_version,
        );
        insert(
            &db,
            &[b"narrow"],
            b"a",
            Element::new_sum_item(i64::MAX),
            grove_version,
        );
        let refused = db
            .insert(
                &[b"narrow".as_slice()],
                b"b",
                Element::new_sum_item(1),
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            refused.is_err(),
            "an i64 total must refuse a value past its range"
        );
    }

    /// Negative totals outside the `i64` range are held as well, and mixed
    /// signs cancel exactly.
    #[test]
    fn big_sum_tree_holds_negative_and_cancelling_totals() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_big_tree(&db, &[], BIG, grove_version);

        for key in [b"a", b"b", b"c"] {
            insert(
                &db,
                &[BIG],
                key,
                Element::new_sum_item(i64::MIN),
                grove_version,
            );
        }
        let three_min = 3 * (i64::MIN as i128);
        assert_eq!(recorded(&db, &[], BIG, grove_version), (3, three_min));
        assert!(three_min < i64::MIN as i128);

        for key in [b"d", b"e", b"f"] {
            insert(
                &db,
                &[BIG],
                key,
                Element::new_sum_item(i64::MAX),
                grove_version,
            );
        }
        // Each pair adds up to -1.
        assert_eq!(recorded(&db, &[], BIG, grove_version), (6, -3));

        insert(
            &db,
            &[BIG],
            b"zero",
            Element::new_sum_item(0),
            grove_version,
        );
        assert_eq!(recorded(&db, &[], BIG, grove_version), (7, -3));
    }

    /// Deleting a child takes its count and its sum back out, across the
    /// `i64` boundary.
    #[test]
    fn deleting_a_child_lowers_the_count_and_the_big_sum() {
        let grove_version = GroveVersion::latest();
        let db = db_with_three_max_items(grove_version);

        db.delete(&[BIG], b"b", None, None, grove_version)
            .unwrap()
            .expect("should delete a child");
        assert_eq!(
            recorded(&db, &[], BIG, grove_version),
            (2, 2 * (i64::MAX as i128))
        );

        db.delete(&[BIG], b"a", None, None, grove_version)
            .unwrap()
            .expect("should delete a child");
        db.delete(&[BIG], b"c", None, None, grove_version)
            .unwrap()
            .expect("should delete a child");
        assert_eq!(recorded(&db, &[], BIG, grove_version), (0, 0));
    }

    /// Each kind of child counts and sums as it would under the narrow tree,
    /// with its sum widened: an item counts one and sums nothing, an item
    /// with a sum and a reference with a sum carry theirs, and a tree
    /// contributes its own count (one when it has none) and its own sum.
    #[test]
    fn each_kind_of_child_contributes_its_count_and_widened_sum() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_big_tree(&db, &[], BIG, grove_version);

        insert(
            &db,
            &[BIG],
            b"item",
            Element::new_item(vec![1]),
            grove_version,
        );
        assert_eq!(recorded(&db, &[], BIG, grove_version), (1, 0));

        insert(
            &db,
            &[BIG],
            b"item_with_sum",
            Element::new_item_with_sum_item(vec![2], 5),
            grove_version,
        );
        assert_eq!(recorded(&db, &[], BIG, grove_version), (2, 5));

        insert(
            &db,
            &[BIG],
            b"reference_with_sum",
            Element::new_reference_with_sum_item(
                ReferencePathType::AbsolutePathReference(vec![BIG.to_vec(), b"item".to_vec()]),
                7,
            ),
            grove_version,
        );
        assert_eq!(recorded(&db, &[], BIG, grove_version), (3, 12));

        // A sum tree counts as one element and contributes its sum.
        insert(
            &db,
            &[BIG],
            b"sum_tree",
            Element::empty_sum_tree(),
            grove_version,
        );
        insert(
            &db,
            &[BIG, b"sum_tree"],
            b"x",
            Element::new_sum_item(i64::MAX),
            grove_version,
        );
        assert_eq!(
            recorded(&db, &[], BIG, grove_version),
            (4, 12 + i64::MAX as i128)
        );

        // A count sum tree contributes its own count and its sum.
        insert(
            &db,
            &[BIG],
            b"count_sum_tree",
            Element::empty_count_sum_tree(),
            grove_version,
        );
        for key in [b"x", b"y"] {
            insert(
                &db,
                &[BIG, b"count_sum_tree"],
                key,
                Element::new_sum_item(10),
                grove_version,
            );
        }
        assert_eq!(
            recorded(&db, &[], BIG, grove_version),
            (6, 32 + i64::MAX as i128)
        );

        // A big sum tree contributes its 128-bit sum.
        insert(
            &db,
            &[BIG],
            b"big_sum_tree",
            Element::empty_big_sum_tree(),
            grove_version,
        );
        for key in [b"x", b"y"] {
            insert(
                &db,
                &[BIG, b"big_sum_tree"],
                key,
                Element::new_sum_item(i64::MAX),
                grove_version,
            );
        }
        assert_eq!(recorded(&db, &[], BIG, grove_version), (7, 32 + THREE_MAX));
    }

    /// A wide tree nested in a wide tree passes both its count and its
    /// 128-bit sum up, however deep it sits.
    #[test]
    fn nested_big_sum_trees_pass_count_and_big_sum_up() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_big_tree(&db, &[], BIG, grove_version);
        insert_big_tree(&db, &[BIG], b"inner", grove_version);
        insert_big_tree(&db, &[BIG, b"inner"], b"innermost", grove_version);

        for key in [b"a", b"b", b"c"] {
            insert(
                &db,
                &[BIG, b"inner", b"innermost"],
                key,
                Element::new_sum_item(i64::MAX),
                grove_version,
            );
        }
        insert(
            &db,
            &[BIG, b"inner"],
            b"d",
            Element::new_sum_item(i64::MAX),
            grove_version,
        );

        assert_eq!(
            recorded(&db, &[BIG, b"inner"], b"innermost", grove_version),
            (3, THREE_MAX)
        );
        assert_eq!(
            recorded(&db, &[BIG], b"inner", grove_version),
            (4, THREE_MAX + i64::MAX as i128)
        );
        assert_eq!(
            recorded(&db, &[], BIG, grove_version),
            (4, THREE_MAX + i64::MAX as i128)
        );
    }

    /// Under a parent whose sum is an `i64` the wide tree contributes its
    /// count and no sum, as a `BigSumTree` does; under a `BigSumTree` it
    /// contributes its sum.
    #[test]
    fn big_sum_tree_contributes_no_sum_to_an_i64_parent() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        insert(
            &db,
            &[],
            b"count_sum",
            Element::empty_count_sum_tree(),
            grove_version,
        );
        insert_big_tree(&db, &[b"count_sum"], BIG, grove_version);
        for key in [b"a", b"b", b"c"] {
            insert(
                &db,
                &[b"count_sum", BIG],
                key,
                Element::new_sum_item(i64::MAX),
                grove_version,
            );
        }
        let parent = db
            .get(&[] as &[&[u8]], b"count_sum", None, grove_version)
            .unwrap()
            .expect("should get the parent");
        assert_eq!(parent.count_sum_value_or_default(), (3, 0));
        assert_eq!(
            recorded(&db, &[b"count_sum"], BIG, grove_version),
            (3, THREE_MAX)
        );

        insert(
            &db,
            &[],
            b"big_sum",
            Element::empty_big_sum_tree(),
            grove_version,
        );
        insert_big_tree(&db, &[b"big_sum"], BIG, grove_version);
        for key in [b"a", b"b", b"c"] {
            insert(
                &db,
                &[b"big_sum", BIG],
                key,
                Element::new_sum_item(i64::MAX),
                grove_version,
            );
        }
        let parent = db
            .get(&[] as &[&[u8]], b"big_sum", None, grove_version)
            .unwrap()
            .expect("should get the parent");
        assert_eq!(parent.big_sum_value_or_default(), THREE_MAX);
    }

    /// The wrappers apply to the wide tree as a child: `NonCounted` hides its
    /// count from a count tree, `NotSummed` its sum from a sum-bearing
    /// parent, and `NotCountedOrSummed` both.
    #[test]
    fn wrapped_big_sum_tree_hides_what_its_wrapper_names() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        insert(
            &db,
            &[],
            b"count",
            Element::empty_count_tree(),
            grove_version,
        );
        insert(
            &db,
            &[b"count"],
            BIG,
            Element::new_non_counted(Element::empty_provable_count_provable_big_sum_tree())
                .expect("a tree can be non counted"),
            grove_version,
        );
        insert(
            &db,
            &[b"count", BIG],
            b"a",
            Element::new_sum_item(i64::MAX),
            grove_version,
        );
        let parent = db
            .get(&[] as &[&[u8]], b"count", None, grove_version)
            .unwrap()
            .expect("should get the parent");
        assert_eq!(parent.count_value_or_default(), 0);
        assert_eq!(
            recorded(&db, &[b"count"], BIG, grove_version),
            (1, i64::MAX as i128)
        );

        insert(
            &db,
            &[],
            b"big_sum",
            Element::empty_big_sum_tree(),
            grove_version,
        );
        insert(
            &db,
            &[b"big_sum"],
            BIG,
            Element::new_not_summed(Element::empty_provable_count_provable_big_sum_tree())
                .expect("a sum-bearing tree can be not summed"),
            grove_version,
        );
        insert(
            &db,
            &[b"big_sum", BIG],
            b"a",
            Element::new_sum_item(i64::MAX),
            grove_version,
        );
        let parent = db
            .get(&[] as &[&[u8]], b"big_sum", None, grove_version)
            .unwrap()
            .expect("should get the parent");
        assert_eq!(parent.big_sum_value_or_default(), 0);

        insert(
            &db,
            &[],
            b"count_sum",
            Element::empty_count_sum_tree(),
            grove_version,
        );
        insert(
            &db,
            &[b"count_sum"],
            BIG,
            Element::new_not_counted_or_summed(
                Element::empty_provable_count_provable_big_sum_tree(),
            )
            .expect("a sum-bearing tree can be not counted or summed"),
            grove_version,
        );
        insert(
            &db,
            &[b"count_sum", BIG],
            b"a",
            Element::new_sum_item(5),
            grove_version,
        );
        let parent = db
            .get(&[] as &[&[u8]], b"count_sum", None, grove_version)
            .unwrap()
            .expect("should get the parent");
        assert_eq!(parent.count_sum_value_or_default(), (0, 0));
    }

    /// The tree commits its count and sum into every node hash, so a child
    /// that hides either from it is refused, as under the other provable
    /// trees.
    #[test]
    fn count_hiding_wrappers_are_refused_under_a_big_sum_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_big_tree(&db, &[], BIG, grove_version);

        let non_counted = db
            .insert(
                &[BIG],
                b"hidden",
                Element::new_non_counted(Element::new_item(vec![1]))
                    .expect("an item can be non counted"),
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert!(non_counted.is_err(), "a non counted child must be refused");

        let not_counted_or_summed = db
            .insert(
                &[BIG],
                b"hidden",
                Element::new_not_counted_or_summed(Element::empty_sum_tree())
                    .expect("a sum tree can be not counted or summed"),
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            not_counted_or_summed.is_err(),
            "a not counted or summed child must be refused"
        );
        assert_eq!(recorded(&db, &[], BIG, grove_version), (0, 0));
    }

    /// The wide tree hashes its nodes with their own function: the same
    /// content gives it another root than the narrow tree.
    #[test]
    fn big_sum_tree_root_hash_diverges_from_the_narrow_tree() {
        let grove_version = GroveVersion::latest();

        let root_hash_with = |tree: Element| {
            let db = make_test_grovedb(grove_version);
            insert(&db, &[], b"t", tree, grove_version);
            for (key, value) in [(b"a", 7i64), (b"b", 13), (b"c", 20)] {
                insert(
                    &db,
                    &[b"t"],
                    key,
                    Element::new_sum_item(value),
                    grove_version,
                );
            }
            db.root_hash(None, grove_version)
                .unwrap()
                .expect("should compute the root hash")
        };

        assert_ne!(
            root_hash_with(Element::empty_provable_count_provable_big_sum_tree()),
            root_hash_with(Element::empty_provable_count_provable_sum_tree())
        );
    }

    /// A batch builds the tree and its children, and the total it leaves
    /// equals the one direct inserts leave, root hash included.
    #[test]
    fn batch_leaves_the_same_tree_as_direct_inserts() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let mut ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![],
            BIG.to_vec(),
            Element::empty_provable_count_provable_big_sum_tree(),
        )];
        for key in [b"a", b"b", b"c"] {
            ops.push(QualifiedGroveDbOp::insert_or_replace_op(
                vec![BIG.to_vec()],
                key.to_vec(),
                Element::new_sum_item(i64::MAX),
            ));
        }
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("should apply the batch");
        assert_eq!(recorded(&db, &[], BIG, grove_version), (3, THREE_MAX));

        let direct = db_with_three_max_items(grove_version);
        assert_eq!(
            db.root_hash(None, grove_version).unwrap().unwrap(),
            direct.root_hash(None, grove_version).unwrap().unwrap()
        );

        // A second batch replaces one child and deletes another.
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![BIG.to_vec()],
                b"a".to_vec(),
                Element::new_sum_item(1),
            ),
            QualifiedGroveDbOp::delete_op(vec![BIG.to_vec()], b"b".to_vec()),
        ];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("should apply the second batch");
        assert_eq!(
            recorded(&db, &[], BIG, grove_version),
            (2, 1 + i64::MAX as i128)
        );
    }

    /// Prove `path_query` and verify it, returning the verified rows. The
    /// proof must verify against the database's own root hash.
    fn prove_and_verify(
        db: &TempGroveDb,
        path_query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> Vec<(Vec<Vec<u8>>, Vec<u8>, Option<Element>)> {
        let root_hash = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should compute the root hash");
        let proof = db
            .prove_query(path_query, None, grove_version)
            .unwrap()
            .expect("should prove the query");
        let (proven_root, rows) =
            GroveDb::verify_query(&proof, path_query, grove_version).expect("should verify");
        assert_eq!(
            proven_root, root_hash,
            "the proof must verify against the database root"
        );
        rows
    }

    /// Items of the tree are proved with their count and big sum bound into
    /// each node hash, over a full range, a limited range and single keys.
    #[test]
    fn items_of_a_big_sum_tree_prove_against_the_root() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_big_tree(&db, &[], BIG, grove_version);
        for index in 0u8..20 {
            insert(
                &db,
                &[BIG],
                &[index],
                Element::new_sum_item(i64::MAX - index as i64),
                grove_version,
            );
        }

        let mut query = Query::new();
        query.insert_all();
        let rows = prove_and_verify(
            &db,
            &PathQuery::new_unsized(vec![BIG.to_vec()], query.clone()),
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

        let limited = PathQuery::new(vec![BIG.to_vec()], SizedQuery::new(query, Some(5), None));
        assert_eq!(prove_and_verify(&db, &limited, grove_version).len(), 5);

        let mut range = Query::new();
        range.insert_item(QueryItem::Range(vec![3]..vec![9]));
        let rows = prove_and_verify(
            &db,
            &PathQuery::new_unsized(vec![BIG.to_vec()], range),
            grove_version,
        );
        assert_eq!(rows.len(), 6);

        let mut descending = Query::new_with_direction(false);
        descending.insert_item(QueryItem::RangeFrom(vec![15]..));
        let rows = prove_and_verify(
            &db,
            &PathQuery::new_unsized(vec![BIG.to_vec()], descending),
            grove_version,
        );
        assert_eq!(
            rows.iter().map(|(_, key, _)| key[0]).collect::<Vec<_>>(),
            vec![19, 18, 17, 16, 15]
        );
    }

    /// A key that is not in the tree is proved absent: the boundary nodes
    /// carry the count and big sum their hashes commit to.
    #[test]
    fn absent_key_of_a_big_sum_tree_proves_against_the_root() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_big_tree(&db, &[], BIG, grove_version);
        for index in [1u8, 3, 5, 7, 9] {
            insert(
                &db,
                &[BIG],
                &[index],
                Element::new_sum_item(i64::MAX),
                grove_version,
            );
        }

        for absent in [0u8, 4, 10] {
            let mut query = Query::new();
            query.insert_key(vec![absent]);
            let path_query =
                PathQuery::new(vec![BIG.to_vec()], SizedQuery::new(query, Some(1), None));
            let root_hash = db.root_hash(None, grove_version).unwrap().unwrap();
            let proof = db
                .prove_query(&path_query, None, grove_version)
                .unwrap()
                .expect("should prove the absence");
            let (proven_root, rows) =
                GroveDb::verify_query_with_absence_proof(&proof, &path_query, grove_version)
                    .expect("should verify the absence");
            assert_eq!(proven_root, root_hash);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].1, vec![absent]);
            assert_eq!(rows[0].2, None);
        }
    }

    /// A reference in the tree is proved with a node carrying the count and
    /// big sum, and the verified row is the value it points at.
    #[test]
    fn reference_in_a_big_sum_tree_proves_against_the_root() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_big_tree(&db, &[], BIG, grove_version);
        insert(&db, &[], b"values", Element::empty_tree(), grove_version);
        insert(
            &db,
            &[b"values"],
            b"target",
            Element::new_item(b"referenced".to_vec()),
            grove_version,
        );
        // Neighbours on both sides, so the reference row has siblings whose
        // nodes carry aggregates too.
        for key in [b"a", b"z"] {
            insert(
                &db,
                &[BIG],
                key,
                Element::new_sum_item(i64::MAX),
                grove_version,
            );
        }
        insert(
            &db,
            &[BIG],
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
        assert_eq!(recorded(&db, &[], BIG, grove_version), (3, THREE_MAX));

        let mut query = Query::new();
        query.insert_key(b"r".to_vec());
        let rows = prove_and_verify(
            &db,
            &PathQuery::new_unsized(vec![BIG.to_vec()], query),
            grove_version,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].2, Some(Element::new_item(b"referenced".to_vec())));

        let mut all = Query::new();
        all.insert_all();
        let rows = prove_and_verify(
            &db,
            &PathQuery::new_unsized(vec![BIG.to_vec()], all),
            grove_version,
        );
        assert_eq!(rows.len(), 3);
    }

    /// A query descends through wide trees nested in a wide tree: each
    /// tree row carries the count and big sum of the tree below it.
    #[test]
    fn subquery_through_nested_big_sum_trees_proves_against_the_root() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_big_tree(&db, &[], BIG, grove_version);
        for outer in [b"x", b"y", b"z"] {
            insert_big_tree(&db, &[BIG], outer, grove_version);
            for inner in [b"1", b"2", b"3"] {
                insert(
                    &db,
                    &[BIG, outer],
                    inner,
                    Element::new_sum_item(i64::MAX),
                    grove_version,
                );
            }
        }
        assert_eq!(
            recorded(&db, &[], BIG, grove_version),
            (9, 9 * (i64::MAX as i128))
        );

        let mut inner = Query::new();
        inner.insert_all();
        let mut outer = Query::new();
        outer.insert_all();
        outer.set_subquery(inner);
        let rows = prove_and_verify(
            &db,
            &PathQuery::new_unsized(vec![BIG.to_vec()], outer),
            grove_version,
        );
        assert_eq!(rows.len(), 9);
        assert!(rows
            .iter()
            .all(|(_, _, element)| element == &Some(Element::new_sum_item(i64::MAX))));

        // The tree elements themselves, without a subquery.
        let mut trees = Query::new();
        trees.insert_all();
        let rows = prove_and_verify(
            &db,
            &PathQuery::new_unsized(vec![BIG.to_vec()], trees),
            grove_version,
        );
        assert_eq!(rows.len(), 3);
        for (_, _, element) in rows {
            let (count, sum) = element
                .expect("a tree row has an element")
                .as_provable_count_provable_big_sum_tree_value()
                .expect("the row is a wide tree");
            assert_eq!((count, sum), (3, THREE_MAX));
        }
    }

    /// A proof whose count or big sum was altered no longer verifies: both
    /// are part of the node hash.
    #[test]
    fn altered_count_or_big_sum_in_a_proof_is_refused() {
        use grovedb_merk::proofs::{encode_into, Decoder, Node, Op};

        let grove_version = GroveVersion::latest();
        let db = db_with_three_max_items(grove_version);

        let mut query = Query::new();
        query.insert_key(b"b".to_vec());
        let path_query = PathQuery::new_unsized(vec![BIG.to_vec()], query);
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove the query");
        GroveDb::verify_query(&proof, &path_query, grove_version).expect("should verify as is");

        let config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        // Rewrite every wide node of the leaf layer with `alter`.
        let altered_proof = |alter: &dyn Fn(u64, i128) -> (u64, i128)| -> Vec<u8> {
            let (mut decoded, _): (GroveDBProof, _) =
                bincode::decode_from_slice(&proof, config).expect("should decode the envelope");
            let GroveDBProof::V1(ref mut v1) = decoded else {
                panic!("the latest version proves with a V1 envelope");
            };
            let leaf = v1
                .root_layer
                .lower_layers
                .get_mut(BIG)
                .expect("the leaf layer is under the tree's key");
            let ProofBytes::Merk(ref mut bytes) = leaf.merk_proof else {
                panic!("a wide tree is a Merk tree");
            };
            let mut seen = 0;
            let ops: Vec<Op> = Decoder::new(bytes)
                .map(|op| op.expect("should decode the op"))
                .map(|op| {
                    let rewrite = |node: Node, seen: &mut i32| match node {
                        Node::KVCountBigSum(key, value, count, sum) => {
                            *seen += 1;
                            let (count, sum) = alter(count, sum);
                            Node::KVCountBigSum(key, value, count, sum)
                        }
                        Node::KVHashCountBigSum(hash, count, sum) => {
                            *seen += 1;
                            let (count, sum) = alter(count, sum);
                            Node::KVHashCountBigSum(hash, count, sum)
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
            assert!(seen > 0, "the leaf layer must hold wide nodes");
            let mut rewritten = Vec::new();
            encode_into(ops.iter(), &mut rewritten);
            *bytes = rewritten;
            bincode::encode_to_vec(decoded, config).expect("should encode the envelope")
        };

        // Unaltered, the re-encoded proof still verifies.
        GroveDb::verify_query(
            &altered_proof(&|count, sum| (count, sum)),
            &path_query,
            grove_version,
        )
        .expect("the re-encoded proof should verify");

        let altered_sum = altered_proof(&|count, sum| (count, sum + 1));
        assert!(
            GroveDb::verify_query(&altered_sum, &path_query, grove_version).is_err(),
            "an altered big sum must be refused"
        );
        let altered_count = altered_proof(&|count, sum| (count + 1, sum));
        assert!(
            GroveDb::verify_query(&altered_count, &path_query, grove_version).is_err(),
            "an altered count must be refused"
        );
    }

    /// A V0 envelope has no node for this tree, so proving through one is
    /// refused instead of dropping the aggregates from the hash chain. The
    /// versions that prove with a V0 envelope cannot create the tree, so it
    /// is created under the latest one.
    #[test]
    fn big_sum_tree_proof_is_refused_on_a_v0_envelope() {
        use grovedb_version::version::v2::GROVE_V2;
        let latest = GroveVersion::latest();

        let db = make_test_grovedb(latest);
        insert_big_tree(&db, &[], BIG, latest);
        insert(&db, &[BIG], b"a", Element::new_item(vec![1, 2, 3]), latest);

        let mut query = Query::new();
        query.insert_key(b"a".to_vec());
        let path_query = PathQuery::new_unsized(vec![BIG.to_vec()], query);
        let err = db
            .prove_query(&path_query, None, &GROVE_V2)
            .unwrap()
            .expect_err("a V0 envelope must refuse the tree");
        assert!(
            matches!(err, Error::NotSupported(ref message) if message.contains("V1 proof envelopes")),
            "expected a V1 envelope refusal, got {err:?}"
        );
    }

    /// The released grove versions do not know the element, so each refuses
    /// to create it, bare or wrapped, by a direct insert and by a batch, and
    /// still creates the narrow tree.
    #[test]
    fn released_grove_versions_refuse_to_create_a_big_sum_tree() {
        use grovedb_version::version::{v1::GROVE_V1, v2::GROVE_V2, v3::GROVE_V3, v4::GROVE_V4};

        for grove_version in [&GROVE_V1, &GROVE_V2, &GROVE_V3, &GROVE_V4] {
            let db = make_test_grovedb(grove_version);
            insert(
                &db,
                &[],
                b"count",
                Element::empty_count_tree(),
                grove_version,
            );

            let is_version_refusal = |result: Result<(), Error>| {
                matches!(
                    result,
                    Err(Error::VersionError(
                        grovedb_version::error::GroveVersionError::UnknownVersionMismatch { .. }
                    ))
                )
            };

            let direct = db
                .insert(
                    &[] as &[&[u8]],
                    BIG,
                    Element::empty_provable_count_provable_big_sum_tree(),
                    None,
                    None,
                    grove_version,
                )
                .unwrap();
            assert!(
                is_version_refusal(direct),
                "protocol version {} must refuse a direct insert",
                grove_version.protocol_version
            );

            let wrapped = db
                .insert(
                    &[b"count".as_slice()],
                    BIG,
                    Element::new_non_counted(Element::empty_provable_count_provable_big_sum_tree())
                        .expect("a tree can be non counted"),
                    None,
                    None,
                    grove_version,
                )
                .unwrap();
            assert!(
                is_version_refusal(wrapped),
                "protocol version {} must refuse a wrapped direct insert",
                grove_version.protocol_version
            );

            let batched = db
                .apply_batch(
                    vec![QualifiedGroveDbOp::insert_or_replace_op(
                        vec![],
                        BIG.to_vec(),
                        Element::empty_provable_count_provable_big_sum_tree(),
                    )],
                    None,
                    None,
                    grove_version,
                )
                .unwrap();
            assert!(
                is_version_refusal(batched),
                "protocol version {} must refuse a batch insert",
                grove_version.protocol_version
            );

            assert!(db
                .get(&[] as &[&[u8]], BIG, None, grove_version)
                .unwrap()
                .is_err());

            insert(
                &db,
                &[],
                b"narrow",
                Element::empty_provable_count_provable_sum_tree(),
                grove_version,
            );
        }
    }

    /// A wide tree of the sum items `0..20`, the item at `index` holding
    /// `i64::MAX - index`.
    fn db_with_twenty_items(grove_version: &GroveVersion) -> TempGroveDb {
        let db = make_test_grovedb(grove_version);
        insert_big_tree(&db, &[], BIG, grove_version);
        for index in 0u8..20 {
            insert(
                &db,
                &[BIG],
                &[index],
                Element::new_sum_item(i64::MAX - index as i64),
                grove_version,
            );
        }
        db
    }

    /// The sum of the items `keys` of [`db_with_twenty_items`].
    fn sum_of(keys: std::ops::Range<u8>) -> i128 {
        keys.map(|index| i64::MAX as i128 - index as i128).sum()
    }

    /// The three range aggregates are proved on the tree and verified
    /// against the database root: the count, the 128-bit sum and both.
    #[test]
    fn range_aggregates_of_a_big_sum_tree_prove_against_the_root() {
        let grove_version = GroveVersion::latest();
        let db = db_with_twenty_items(grove_version);
        let root_hash = db.root_hash(None, grove_version).unwrap().unwrap();

        for (inner, keys) in [
            (QueryItem::RangeTo(..vec![4]), 0u8..4),
            (QueryItem::RangeInclusive(vec![0]..=vec![19]), 0..20),
            (QueryItem::Range(vec![3]..vec![9]), 3..9),
            (QueryItem::RangeInclusive(vec![3]..=vec![9]), 3..10),
            (QueryItem::RangeFrom(vec![15]..), 15..20),
            (QueryItem::Range(vec![30]..vec![40]), 0..0),
        ] {
            let count = keys.len() as u64;
            let sum = sum_of(keys);

            let path_query = PathQuery::new_unsized(
                vec![BIG.to_vec()],
                Query::new_aggregate_count_on_range(inner.clone()),
            );
            let proof = db
                .prove_query(&path_query, None, grove_version)
                .unwrap()
                .expect("should prove the count");
            assert_eq!(
                GroveDb::verify_aggregate_count_query(&proof, &path_query, grove_version)
                    .expect("should verify the count"),
                (root_hash, count)
            );

            let path_query = PathQuery::new_unsized(
                vec![BIG.to_vec()],
                Query::new_aggregate_sum_on_range(inner.clone()),
            );
            let proof = db
                .prove_query(&path_query, None, grove_version)
                .unwrap()
                .expect("should prove the sum");
            assert_eq!(
                GroveDb::verify_aggregate_big_sum_query(&proof, &path_query, grove_version)
                    .expect("should verify the sum"),
                (root_hash, sum)
            );
            // The i64 entry point refuses a proof of the wide tree.
            assert!(
                GroveDb::verify_aggregate_sum_query(&proof, &path_query, grove_version).is_err()
            );

            let path_query = PathQuery::new_unsized(
                vec![BIG.to_vec()],
                Query::new_aggregate_count_and_sum_on_range(inner.clone()),
            );
            let proof = db
                .prove_query(&path_query, None, grove_version)
                .unwrap()
                .expect("should prove the count and sum");
            assert_eq!(
                GroveDb::verify_aggregate_count_and_big_sum_query(
                    &proof,
                    &path_query,
                    grove_version
                )
                .expect("should verify the count and sum"),
                (root_hash, count, sum)
            );
            assert!(GroveDb::verify_aggregate_count_and_sum_query(
                &proof,
                &path_query,
                grove_version
            )
            .is_err());
        }
    }

    /// `verify_grovedb` recomputes each tree's aggregate from its elements
    /// and compares it with the stored one: a wide tree, its nested trees
    /// and its mixed children agree.
    #[test]
    fn verify_grovedb_finds_a_big_sum_tree_consistent() {
        let grove_version = GroveVersion::latest();
        let db = db_with_three_max_items(grove_version);
        insert_big_tree(&db, &[BIG], b"inner", grove_version);
        for key in [b"a", b"b"] {
            insert(
                &db,
                &[BIG, b"inner"],
                key,
                Element::new_sum_item(i64::MIN),
                grove_version,
            );
        }
        insert(
            &db,
            &[BIG],
            b"sum_tree",
            Element::empty_sum_tree(),
            grove_version,
        );
        insert(
            &db,
            &[BIG, b"sum_tree"],
            b"a",
            Element::new_sum_item(9),
            grove_version,
        );

        let issues = db
            .verify_grovedb(None, true, false, grove_version)
            .expect("should verify the database");
        assert!(issues.is_empty(), "unexpected issues: {issues:?}");
    }

    /// The tree's count and big sum are read without a proof as their own
    /// result kind.
    #[test]
    fn query_without_a_proof_returns_the_count_and_big_sum() {
        use crate::operations::get::QueryItemOrSumReturnType;

        let grove_version = GroveVersion::latest();
        let db = db_with_three_max_items(grove_version);

        let mut query = Query::new();
        query.insert_key(BIG.to_vec());
        let (results, _) = db
            .query_item_value_or_sum(
                &PathQuery::new_unsized(vec![], query),
                true,
                true,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should query the tree");
        assert_eq!(
            results,
            vec![QueryItemOrSumReturnType::CountBigSumValue(3, THREE_MAX)]
        );
    }

    /// A count-bearing tree serves trunk and branch chunk proofs: the trunk
    /// verifies, each leaf it names carries a count, and every branch
    /// verifies against the hash the trunk committed to.
    #[test]
    fn trunk_and_branch_chunks_of_a_big_sum_tree_verify() {
        use crate::query::{PathBranchChunkQuery, PathTrunkChunkQuery};

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_big_tree(&db, &[], BIG, grove_version);
        for index in 0u8..100 {
            insert(
                &db,
                &[BIG],
                &[index],
                Element::new_item_with_sum_item(vec![index; 10], i64::MAX),
                grove_version,
            );
        }
        assert_eq!(
            recorded(&db, &[], BIG, grove_version),
            (100, 100 * (i64::MAX as i128))
        );

        let trunk_query = PathTrunkChunkQuery::new(vec![BIG.to_vec()], 5);
        let trunk_proof = db
            .prove_trunk_chunk(&trunk_query, grove_version)
            .unwrap()
            .expect("should prove the trunk");
        let (root_hash, trunk) =
            GroveDb::verify_trunk_chunk_proof(&trunk_proof, &trunk_query, grove_version)
                .expect("should verify the trunk");
        assert_eq!(
            root_hash,
            db.root_hash(None, grove_version).unwrap().unwrap()
        );
        assert_eq!(trunk.elements.len(), 31);
        assert!(!trunk.leaf_keys.is_empty());

        let remaining_depth = *trunk
            .chunk_depths
            .get(1)
            .expect("a tree of 100 elements needs a second chunk level");
        let mut from_branches = 0;
        let mut counted = 0;
        for (leaf_key, leaf_info) in &trunk.leaf_keys {
            counted += leaf_info
                .count
                .expect("a leaf of a count-bearing tree carries its count");
            let branch_query =
                PathBranchChunkQuery::new(vec![BIG.to_vec()], leaf_key.clone(), remaining_depth);
            let branch_proof = db
                .prove_branch_chunk(&branch_query, grove_version)
                .unwrap()
                .expect("should prove the branch");
            let branch = GroveDb::verify_branch_chunk_proof(
                &branch_proof,
                &branch_query,
                leaf_info.hash,
                grove_version,
            )
            .expect("should verify the branch");
            assert_eq!(branch.branch_root_hash, leaf_info.hash);
            from_branches += branch.elements.len();
        }
        // Each branch root is also a leaf of the trunk.
        assert_eq!(
            trunk.elements.len() + from_branches - trunk.leaf_keys.len(),
            100
        );
        // The leaves' subtrees hold every element the trunk does not.
        assert_eq!(
            counted as usize,
            100 - trunk.elements.len() + trunk.leaf_keys.len()
        );
    }

    /// The range aggregates are read without a proof to the same values the
    /// proofs give, through the big sum entry points; the `i64` ones keep
    /// refusing the tree.
    #[test]
    fn range_aggregates_of_a_big_sum_tree_are_read_without_a_proof() {
        let grove_version = GroveVersion::latest();
        let db = db_with_twenty_items(grove_version);
        let inner = QueryItem::Range(vec![3]..vec![9]);
        let sum = sum_of(3..9);

        let count_query = PathQuery::new_unsized(
            vec![BIG.to_vec()],
            Query::new_aggregate_count_on_range(inner.clone()),
        );
        assert_eq!(
            db.query_aggregate_count(&count_query, None, grove_version)
                .unwrap()
                .expect("should read the count"),
            6
        );

        let sum_query = PathQuery::new_unsized(
            vec![BIG.to_vec()],
            Query::new_aggregate_sum_on_range(inner.clone()),
        );
        assert_eq!(
            db.query_aggregate_big_sum(&sum_query, None, grove_version)
                .unwrap()
                .expect("should read the sum"),
            sum
        );
        assert_eq!(
            db.query_aggregate_big_sum_per_key(&sum_query, None, grove_version)
                .unwrap()
                .expect("should read the sum per key"),
            vec![(Vec::new(), sum)]
        );
        assert!(db
            .query_aggregate_sum(&sum_query, None, grove_version)
            .unwrap()
            .is_err());

        let both_query = PathQuery::new_unsized(
            vec![BIG.to_vec()],
            Query::new_aggregate_count_and_sum_on_range(inner),
        );
        assert_eq!(
            db.query_aggregate_count_and_big_sum(&both_query, None, grove_version)
                .unwrap()
                .expect("should read the count and sum"),
            (6, sum)
        );
        assert_eq!(
            db.query_aggregate_count_and_big_sum_per_key(&both_query, None, grove_version)
                .unwrap()
                .expect("should read the count and sum per key"),
            vec![(Vec::new(), 6, sum)]
        );
        assert!(db
            .query_aggregate_count_and_sum(&both_query, None, grove_version)
            .unwrap()
            .is_err());
    }

    /// The big sum entry points refuse a tree whose sum is an `i64`, proved
    /// or not: the caller names the width, and a mismatch is an error
    /// rather than a narrowed or widened answer.
    #[test]
    fn big_sum_range_aggregates_refuse_a_narrow_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert(
            &db,
            &[],
            b"narrow",
            Element::empty_provable_count_provable_sum_tree(),
            grove_version,
        );
        for index in 0u8..10 {
            insert(
                &db,
                &[b"narrow"],
                &[index],
                Element::new_sum_item(index as i64),
                grove_version,
            );
        }
        let inner = QueryItem::Range(vec![3]..vec![9]);

        let sum_query = PathQuery::new_unsized(
            vec![b"narrow".to_vec()],
            Query::new_aggregate_sum_on_range(inner.clone()),
        );
        assert!(db
            .query_aggregate_big_sum(&sum_query, None, grove_version)
            .unwrap()
            .is_err());
        let proof = db
            .prove_query(&sum_query, None, grove_version)
            .unwrap()
            .expect("should prove the narrow sum");
        GroveDb::verify_aggregate_sum_query(&proof, &sum_query, grove_version)
            .expect("the i64 entry point verifies it");
        assert!(
            GroveDb::verify_aggregate_big_sum_query(&proof, &sum_query, grove_version).is_err()
        );

        let both_query = PathQuery::new_unsized(
            vec![b"narrow".to_vec()],
            Query::new_aggregate_count_and_sum_on_range(inner),
        );
        assert!(db
            .query_aggregate_count_and_big_sum(&both_query, None, grove_version)
            .unwrap()
            .is_err());
        let proof = db
            .prove_query(&both_query, None, grove_version)
            .unwrap()
            .expect("should prove the narrow count and sum");
        assert!(GroveDb::verify_aggregate_count_and_big_sum_query(
            &proof,
            &both_query,
            grove_version
        )
        .is_err());
    }

    /// A grove with a plain `groups` tree whose children `x`, `y` and `z`
    /// each hold a wide tree `values` of the sum items `0..count`, the item
    /// at `index` holding `i64::MAX - index`.
    fn db_with_groups(grove_version: &GroveVersion) -> TempGroveDb {
        let db = make_test_grovedb(grove_version);
        insert(&db, &[], b"groups", Element::empty_tree(), grove_version);
        for (group, count) in [(b"x", 4u8), (b"y", 8), (b"z", 12)] {
            insert(
                &db,
                &[b"groups"],
                group,
                Element::empty_tree(),
                grove_version,
            );
            insert_big_tree(&db, &[b"groups", group], b"values", grove_version);
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
        db
    }

    /// A query that fans out over the groups and aggregates the range
    /// `[2, 10)` of each group's wide tree.
    fn per_group_query(aggregate: Query) -> PathQuery {
        let mut carrier = Query::new();
        for group in [b"x", b"y", b"z"] {
            carrier.insert_key(group.to_vec());
        }
        carrier.set_subquery_path(vec![b"values".to_vec()]);
        carrier.set_subquery(aggregate);
        PathQuery::new(
            vec![b"groups".to_vec()],
            SizedQuery::new(carrier, None, None),
        )
    }

    /// A range aggregate per outer key, each over a wide tree: proved and
    /// verified against the root, and read without a proof to the same
    /// values.
    #[test]
    fn per_key_range_aggregates_over_big_sum_trees() {
        let grove_version = GroveVersion::latest();
        let db = db_with_groups(grove_version);
        let root_hash = db.root_hash(None, grove_version).unwrap().unwrap();
        let inner = QueryItem::Range(vec![2]..vec![10]);
        // The groups hold 4, 8 and 12 items: 2, 6 and 8 of them in range.
        let expected: Vec<(Vec<u8>, u64, i128)> = vec![
            (b"x".to_vec(), 2, sum_of(2..4)),
            (b"y".to_vec(), 6, sum_of(2..8)),
            (b"z".to_vec(), 8, sum_of(2..10)),
        ];
        let expected_sums: Vec<(Vec<u8>, i128)> = expected
            .iter()
            .map(|(key, _, sum)| (key.clone(), *sum))
            .collect();

        let sum_query = per_group_query(Query::new_aggregate_sum_on_range(inner.clone()));
        let proof = db
            .prove_query(&sum_query, None, grove_version)
            .unwrap()
            .expect("should prove the sums");
        assert_eq!(
            GroveDb::verify_aggregate_big_sum_query_per_key(&proof, &sum_query, grove_version)
                .expect("should verify the sums"),
            (root_hash, expected_sums.clone())
        );
        assert!(
            GroveDb::verify_aggregate_sum_query_per_key(&proof, &sum_query, grove_version).is_err()
        );
        assert_eq!(
            db.query_aggregate_big_sum_per_key(&sum_query, None, grove_version)
                .unwrap()
                .expect("should read the sums"),
            expected_sums
        );

        let both_query =
            per_group_query(Query::new_aggregate_count_and_sum_on_range(inner.clone()));
        let proof = db
            .prove_query(&both_query, None, grove_version)
            .unwrap()
            .expect("should prove the counts and sums");
        assert_eq!(
            GroveDb::verify_aggregate_count_and_big_sum_query_per_key(
                &proof,
                &both_query,
                grove_version
            )
            .expect("should verify the counts and sums"),
            (root_hash, expected.clone())
        );
        assert!(GroveDb::verify_aggregate_count_and_sum_query_per_key(
            &proof,
            &both_query,
            grove_version
        )
        .is_err());
        assert_eq!(
            db.query_aggregate_count_and_big_sum_per_key(&both_query, None, grove_version)
                .unwrap()
                .expect("should read the counts and sums"),
            expected
        );

        let count_query = per_group_query(Query::new_aggregate_count_on_range(inner));
        let proof = db
            .prove_query(&count_query, None, grove_version)
            .unwrap()
            .expect("should prove the counts");
        assert_eq!(
            GroveDb::verify_aggregate_count_query_per_key(&proof, &count_query, grove_version)
                .expect("should verify the counts"),
            (
                root_hash,
                vec![(b"x".to_vec(), 2), (b"y".to_vec(), 6), (b"z".to_vec(), 8)]
            )
        );
    }

    /// A proved range sum whose proof was altered no longer verifies.
    #[test]
    fn altered_range_aggregate_proof_of_a_big_sum_tree_is_refused() {
        let grove_version = GroveVersion::latest();
        let db = db_with_twenty_items(grove_version);
        let root_hash = db.root_hash(None, grove_version).unwrap().unwrap();
        let path_query = PathQuery::new_unsized(
            vec![BIG.to_vec()],
            Query::new_aggregate_count_and_sum_on_range(QueryItem::Range(vec![3]..vec![9])),
        );
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove the count and sum");

        // Flip each byte of the proof in turn: no altered proof may verify
        // to the database root with another answer.
        let honest =
            GroveDb::verify_aggregate_count_and_big_sum_query(&proof, &path_query, grove_version)
                .expect("should verify as is");
        for index in 0..proof.len() {
            let mut altered = proof.clone();
            altered[index] ^= 0x01;
            if let Ok(result) = GroveDb::verify_aggregate_count_and_big_sum_query(
                &altered,
                &path_query,
                grove_version,
            ) {
                assert!(
                    result.0 != root_hash || result == honest,
                    "byte {index}: an altered proof verified to the root with another answer"
                );
            }
        }
    }

    /// A count-offset page of the tree skips the offset and returns the
    /// rows after it, in both directions, verified against the root.
    #[test]
    fn count_offset_pages_of_a_big_sum_tree_prove_against_the_root() {
        let grove_version = GroveVersion::latest();
        let db = db_with_twenty_items(grove_version);
        let root_hash = db.root_hash(None, grove_version).unwrap().unwrap();

        for (left_to_right, expected_keys) in [(true, vec![5u8, 6, 7]), (false, vec![14u8, 13, 12])]
        {
            let mut query = Query::new_with_direction(left_to_right);
            query.insert_range_inclusive(vec![0]..=vec![19]);
            let path_query =
                PathQuery::new(vec![BIG.to_vec()], SizedQuery::new(query, Some(3), Some(5)));
            let proof = db
                .prove_query(&path_query, None, grove_version)
                .unwrap()
                .expect("should prove the page");
            let (proven_root, rows) = GroveDb::verify_query_raw(&proof, &path_query, grove_version)
                .expect("should verify the page");
            assert_eq!(proven_root, root_hash);
            assert_eq!(
                rows.iter().map(|row| row.key[0]).collect::<Vec<_>>(),
                expected_keys
            );
        }
    }

    /// A count-offset page that returns a reference row: the row is carried
    /// by the wide reference node, and verifies to the value it points at.
    #[test]
    fn count_offset_page_of_a_big_sum_tree_returns_a_reference_row() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_big_tree(&db, &[], BIG, grove_version);
        insert(&db, &[], b"values", Element::empty_tree(), grove_version);
        insert(
            &db,
            &[b"values"],
            b"target",
            Element::new_item(b"referenced".to_vec()),
            grove_version,
        );
        for index in 0u8..10 {
            let element = if index == 6 {
                Element::new_reference_with_sum_item(
                    ReferencePathType::AbsolutePathReference(vec![
                        b"values".to_vec(),
                        b"target".to_vec(),
                    ]),
                    i64::MAX,
                )
            } else {
                Element::new_sum_item(i64::MAX)
            };
            insert(&db, &[BIG], &[index], element, grove_version);
        }
        let root_hash = db.root_hash(None, grove_version).unwrap().unwrap();

        let mut query = Query::new();
        query.insert_range_inclusive(vec![0]..=vec![9]);
        let path_query =
            PathQuery::new(vec![BIG.to_vec()], SizedQuery::new(query, Some(3), Some(5)));
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove the page");
        let (proven_root, rows) = GroveDb::verify_query(&proof, &path_query, grove_version)
            .expect("should verify the page");
        assert_eq!(proven_root, root_hash);
        assert_eq!(
            rows.iter().map(|(_, key, _)| key[0]).collect::<Vec<_>>(),
            vec![5, 6, 7]
        );
        assert_eq!(rows[1].2, Some(Element::new_item(b"referenced".to_vec())));
    }

    /// A local offset pages each wide tree a query fans out over: every
    /// group skips its own first rows, read without a proof and proved to
    /// the same rows.
    #[test]
    fn local_offset_pages_each_big_sum_tree() {
        use crate::query_result_type::QueryResultType;

        let grove_version = GroveVersion::latest();
        let db = db_with_groups(grove_version);
        let root_hash = db.root_hash(None, grove_version).unwrap().unwrap();

        let mut page = Query::new_range_full();
        page.offset = Some(2);
        page.limit = Some(3);
        let mut carrier = Query::new();
        for group in [b"x", b"y", b"z"] {
            carrier.insert_key(group.to_vec());
        }
        carrier.set_subquery_path(vec![b"values".to_vec()]);
        carrier.set_subquery(page);
        let path_query = PathQuery::new(
            vec![b"groups".to_vec()],
            SizedQuery::new(carrier, None, None),
        );

        // The groups hold 4, 8 and 12 items: after skipping two, `x` has two
        // left and the others fill their page of three.
        let expected: Vec<(Vec<u8>, u8)> = vec![
            (b"x".to_vec(), 2),
            (b"x".to_vec(), 3),
            (b"y".to_vec(), 2),
            (b"y".to_vec(), 3),
            (b"y".to_vec(), 4),
            (b"z".to_vec(), 2),
            (b"z".to_vec(), 3),
            (b"z".to_vec(), 4),
        ];

        let (elements, _) = db
            .query(
                &path_query,
                true,
                true,
                true,
                QueryResultType::QueryPathKeyElementTrioResultType,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should read the pages");
        let read: Vec<(Vec<u8>, u8)> = elements
            .to_path_key_elements()
            .into_iter()
            .map(|(path, key, _)| (path[1].clone(), key[0]))
            .collect();
        assert_eq!(read, expected);

        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove the pages");
        let (proven_root, rows) = GroveDb::verify_query(&proof, &path_query, grove_version)
            .expect("should verify the pages");
        assert_eq!(proven_root, root_hash);
        let proved: Vec<(Vec<u8>, u8)> = rows
            .into_iter()
            .map(|(path, key, _)| (path[1].clone(), key[0]))
            .collect();
        assert_eq!(proved, expected);
    }
}
