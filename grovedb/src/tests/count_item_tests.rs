//! End-to-end tests for `Element::CountItem`: an item that carries an
//! explicit count and contributes it, in place of the one any other item
//! counts as, to its count-bearing parent.
//!
//! The element exists so a counter can be kept in place: a write replaces
//! `CountItem(n)` with `CountItem(n ± 1)` and moves every aggregate above it,
//! including an indexed parent's count ranking, without storing a new node.

#[cfg(test)]
mod tests {
    use grovedb_merk::{
        proofs::{
            query::{IndexAxis, QueryItem},
            Query,
        },
        tree_type::COUNT_ITEM_COST_SIZE,
    };
    use grovedb_version::version::{v1::GROVE_V1, v2::GROVE_V2, v3::GROVE_V3, GroveVersion};

    use crate::{
        batch::QualifiedGroveDbOp,
        operations::{
            get::QueryItemOrSumReturnType,
            proof::{indexed_axis::AxisEntries, VerifiedPathQuery},
        },
        tests::{make_test_grovedb, TEST_LEAF},
        Element, Error, GroveDb, PathQuery, SizedQuery,
    };

    fn assert_verify_passes(db: &GroveDb, grove_version: &GroveVersion) {
        let issues = db
            .verify_grovedb(None, true, true, grove_version)
            .expect("verify_grovedb must not return a hard error");
        assert!(
            issues.is_empty(),
            "verify_grovedb reported issues: {issues:?}"
        );
    }

    fn insert(db: &GroveDb, path: &[&[u8]], key: &[u8], element: Element, v: &GroveVersion) {
        db.insert(path, key, element, None, None, v)
            .unwrap()
            .expect("insert");
    }

    fn parent_count(db: &GroveDb, key: &[u8], v: &GroveVersion) -> u64 {
        db.get([TEST_LEAF].as_ref(), key, None, v)
            .unwrap()
            .expect("get parent tree")
            .count_value_or_default()
    }

    #[test]
    fn should_contribute_its_count_to_every_count_tree() {
        let v = GroveVersion::latest();
        for parent in [
            Element::empty_count_tree(),
            Element::empty_count_sum_tree(),
            Element::empty_provable_count_tree(),
            Element::empty_provable_count_sum_tree(),
        ] {
            let db = make_test_grovedb(v);
            insert(&db, &[TEST_LEAF], b"ct", parent.clone(), v);
            insert(
                &db,
                &[TEST_LEAF, b"ct"],
                b"a",
                Element::new_count_item(5),
                v,
            );
            insert(
                &db,
                &[TEST_LEAF, b"ct"],
                b"b",
                Element::new_item(b"x".to_vec()),
                v,
            );
            insert(
                &db,
                &[TEST_LEAF, b"ct"],
                b"c",
                Element::new_count_item(0),
                v,
            );
            assert_eq!(
                parent_count(&db, b"ct", v),
                6,
                "a CountItem adds its count, an Item adds one: {parent:?}"
            );
            assert_verify_passes(&db, v);
        }
    }

    #[test]
    fn should_move_the_parent_count_when_replaced_in_place_without_new_storage() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        insert(&db, &[TEST_LEAF], b"ct", Element::empty_count_tree(), v);
        insert(
            &db,
            &[TEST_LEAF, b"ct"],
            b"counter",
            Element::new_count_item(1),
            v,
        );

        // 100_000 takes more varint bytes than 1, yet the item is charged
        // its fixed size, so the replace stores nothing new.
        let cost = db
            .insert(
                [TEST_LEAF, b"ct"].as_ref(),
                b"counter",
                Element::new_count_item(100_000),
                None,
                None,
                v,
            )
            .cost_as_result()
            .expect("replace count item");
        assert_eq!(cost.storage_cost.added_bytes, 0, "{cost:?}");
        assert_eq!(parent_count(&db, b"ct", v), 100_000);

        // The batch path, which Drive writes through, charges the same.
        let cost = db
            .apply_batch(
                vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
                    b"counter".to_vec(),
                    Element::new_count_item(u64::MAX / 2),
                )],
                None,
                None,
                v,
            )
            .cost_as_result()
            .expect("batch replace count item");
        assert_eq!(cost.storage_cost.added_bytes, 0, "{cost:?}");
        assert_eq!(parent_count(&db, b"ct", v), u64::MAX / 2);

        db.delete([TEST_LEAF, b"ct"].as_ref(), b"counter", None, None, v)
            .unwrap()
            .expect("delete count item");
        assert_eq!(parent_count(&db, b"ct", v), 0);
        assert_verify_passes(&db, v);
    }

    #[test]
    fn should_charge_a_fixed_size_covering_its_largest_encoding() {
        let v = GroveVersion::latest();
        let largest = Element::new_count_item(u64::MAX)
            .serialized_size(v)
            .unwrap() as u32;
        assert!(
            largest <= COUNT_ITEM_COST_SIZE,
            "CountItem(u64::MAX) encodes in {largest} bytes, above the {COUNT_ITEM_COST_SIZE} \
             it is charged"
        );
    }

    #[test]
    fn should_prove_counts_in_a_provable_count_tree() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        insert(
            &db,
            &[TEST_LEAF],
            b"pct",
            Element::empty_provable_count_tree(),
            v,
        );
        for (key, count) in [(b"a", 10), (b"b", 20), (b"c", 30), (b"d", 40), (b"e", 50)] {
            insert(
                &db,
                &[TEST_LEAF, b"pct"],
                key,
                Element::new_count_item(count),
                v,
            );
        }
        let root = db.root_hash(None, v).unwrap().expect("root");

        // A single key: the element comes back with its count and the
        // proof reproduces the root.
        let mut query = Query::new();
        query.insert_key(b"c".to_vec());
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"pct".to_vec()],
            SizedQuery::new(query, None, None),
        );
        let proof = db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove");
        let (proved_root, results) =
            GroveDb::verify_query(&proof, &path_query, v).expect("verify key proof");
        assert_eq!(proved_root, root);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].2, Some(Element::new_count_item(30)));

        // A range: the aggregate is the sum of the counts in range.
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"pct".to_vec()],
            QueryItem::RangeInclusive(b"b".to_vec()..=b"d".to_vec()),
        );
        let proof = db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove");
        let (proved_root, count) = GroveDb::verify_aggregate_count_query(&proof, &path_query, v)
            .expect("verify aggregate count proof");
        assert_eq!(proved_root, root);
        assert_eq!(count, 20 + 30 + 40);
    }

    fn top_counts(db: &GroveDb, limit: u16, v: &GroveVersion) -> Vec<(u64, Vec<u8>)> {
        let path_query = PathQuery::new_axis_top_k(
            vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
            IndexAxis::Count,
            limit,
            0,
            true,
        );
        let proof = db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove");
        let VerifiedPathQuery::AxisEntries {
            root_hash, entries, ..
        } = GroveDb::verify_path_query(&proof, &path_query, v).expect("verify top-k")
        else {
            panic!("expected axis entries");
        };
        assert_eq!(root_hash, db.root_hash(None, v).unwrap().expect("root"));
        let AxisEntries::Count(entries) = entries else {
            panic!("expected count entries, got {entries:?}");
        };
        entries.into_iter().map(|entry| entry.key_pair()).collect()
    }

    #[test]
    fn should_rank_by_its_count_in_a_provable_count_indexed_tree() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        insert(
            &db,
            &[TEST_LEAF],
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            v,
        );
        for (key, count) in [(b"alice".as_slice(), 3), (b"bob", 10), (b"carol", 7)] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                key,
                Element::new_count_item(count),
                None,
                v,
            )
            .unwrap()
            .expect("insert count item into PCIT");
        }
        assert_eq!(parent_count(&db, b"cidx", v), 20);
        assert_eq!(
            top_counts(&db, 2, v),
            vec![(10, b"bob".to_vec()), (7, b"carol".to_vec())]
        );

        // Rewriting a counter in a batch re-keys the ranking.
        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                b"bob".to_vec(),
                Element::new_count_item(1),
            )],
            None,
            None,
            v,
        )
        .unwrap()
        .expect("replace bob's counter");
        assert_eq!(parent_count(&db, b"cidx", v), 11);
        assert_eq!(
            top_counts(&db, 3, v),
            vec![
                (7, b"carol".to_vec()),
                (3, b"alice".to_vec()),
                (1, b"bob".to_vec())
            ]
        );
        assert_verify_passes(&db, v);
    }

    #[test]
    fn should_commit_the_same_root_through_a_batch_and_a_direct_insert() {
        let v = GroveVersion::latest();
        // One item per tree, so both trees take the same shape (a batch
        // inserts in key order, which reshapes a multi-key tree).
        let direct = make_test_grovedb(v);
        insert(
            &direct,
            &[TEST_LEAF],
            b"pct",
            Element::empty_provable_count_tree(),
            v,
        );
        insert(
            &direct,
            &[TEST_LEAF, b"pct"],
            b"a",
            Element::new_count_item(4),
            v,
        );

        let batched = make_test_grovedb(v);
        insert(
            &batched,
            &[TEST_LEAF],
            b"pct",
            Element::empty_provable_count_tree(),
            v,
        );
        batched
            .apply_batch(
                vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"pct".to_vec()],
                    b"a".to_vec(),
                    Element::new_count_item(4),
                )],
                None,
                None,
                v,
            )
            .unwrap()
            .expect("batch insert count item");

        assert_eq!(
            direct.root_hash(None, v).unwrap().expect("root"),
            batched.root_hash(None, v).unwrap().expect("root")
        );
        assert_eq!(parent_count(&batched, b"pct", v), 4);
    }

    #[test]
    fn should_read_back_as_a_count_value() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        insert(&db, &[TEST_LEAF], b"ct", Element::empty_count_tree(), v);
        insert(
            &db,
            &[TEST_LEAF, b"ct"],
            b"k",
            Element::new_count_item(12),
            v,
        );

        let mut query = Query::new();
        query.insert_all();
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            SizedQuery::new(query, None, None),
        );
        let (values, _) = db
            .query_item_value_or_sum(&path_query, true, true, true, None, v)
            .unwrap()
            .expect("query item value or sum");
        assert_eq!(values, vec![QueryItemOrSumReturnType::CountValue(12)]);
    }

    #[test]
    fn should_refuse_a_parent_that_counts_nothing() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        insert(&db, &[TEST_LEAF], b"st", Element::empty_sum_tree(), v);
        for path in [[TEST_LEAF].as_slice(), [TEST_LEAF, b"st"].as_slice()] {
            assert!(db
                .insert(path, b"k", Element::new_count_item(1), None, None, v)
                .unwrap()
                .is_err());
            assert!(db
                .apply_batch(
                    vec![QualifiedGroveDbOp::insert_or_replace_op(
                        path.iter().map(|segment| segment.to_vec()).collect(),
                        b"k".to_vec(),
                        Element::new_count_item(1),
                    )],
                    None,
                    None,
                    v,
                )
                .unwrap()
                .is_err());
        }
    }

    #[test]
    fn should_be_refused_before_grove_v4() {
        for v in [&GROVE_V1, &GROVE_V2, &GROVE_V3] {
            let db = make_test_grovedb(v);
            insert(&db, &[TEST_LEAF], b"ct", Element::empty_count_tree(), v);

            let direct = db
                .insert(
                    [TEST_LEAF, b"ct"].as_ref(),
                    b"k",
                    Element::new_count_item(1),
                    None,
                    None,
                    v,
                )
                .unwrap();
            assert!(matches!(direct, Err(Error::NotSupported(_))), "{direct:?}");

            let batched = db
                .apply_batch(
                    vec![QualifiedGroveDbOp::insert_or_replace_op(
                        vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
                        b"k".to_vec(),
                        Element::new_count_item(1),
                    )],
                    None,
                    None,
                    v,
                )
                .unwrap();
            assert!(
                matches!(batched, Err(Error::NotSupported(_))),
                "{batched:?}"
            );
            assert_eq!(parent_count(&db, b"ct", v), 0);
        }
    }
}
