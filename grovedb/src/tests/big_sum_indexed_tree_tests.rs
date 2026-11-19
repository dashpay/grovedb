//! `ProvableBigSumIndexedTree` and `ProvableCountProvableBigSumIndexedTree`:
//! the indexed trees whose sum is 128 bits wide.
//!
//! An entry's sum reaches past an `i64` when the entry is itself a big sum
//! tree, so the entries here are subtrees holding sum items. The tests rank
//! them through the big sum axis (`IndexAxis::BigSum`), read and proved,
//! and check that the trees behave as their 64-bit counterparts do in
//! every other respect: the mirror follows inserts, updates and removals,
//! a batch commits the root a direct write commits, a removed tree leaves
//! no rows behind, and creation is gated to the grove version that
//! introduces them.

#[cfg(test)]
mod tests {
    use grovedb_element::indexed::{compute_big_avg_fixed_point, IndexAxis};
    use grovedb_merk::proofs::query::{AggregateFold, AxisQuery};
    use grovedb_version::version::GroveVersion;

    use crate::{
        batch::QualifiedGroveDbOp,
        bidirectional_references::BackwardsReferences,
        operations::{
            delete::DeleteOptions,
            get::{AxisAggregateValue, PathQueryRun},
            proof::{indexed_axis::AxisEntries, VerifiedPathQuery},
        },
        query_result_type::{AxisKeys, QueryResultType},
        tests::{make_test_grovedb, TEST_LEAF},
        Element, Error, GroveDb, PathQuery,
    };

    const WIDE: &[u8] = b"wide";
    const MAX: i128 = i64::MAX as i128;
    const MIN: i128 = i64::MIN as i128;

    fn path() -> Vec<Vec<u8>> {
        vec![TEST_LEAF.to_vec(), WIDE.to_vec()]
    }

    fn wide_axes(axes: &[IndexAxis]) -> Element {
        Element::empty_provable_count_provable_big_sum_indexed_tree(
            axes.iter().map(|axis| (axis.tag(), None)).collect(),
        )
        .expect("axes are canonical")
    }

    /// The entries every test ranks: `(key, the sum items of its subtree)`.
    /// Two sums are past `i64::MAX`, one is below `i64::MIN`, and one
    /// subtree is empty.
    fn entries() -> Vec<(&'static [u8], Vec<i64>)> {
        vec![
            (b"a", vec![i64::MAX, i64::MAX]),
            (b"b", vec![i64::MAX, i64::MAX, i64::MAX]),
            (b"c", vec![5]),
            (b"d", vec![i64::MIN, i64::MIN]),
            (b"e", vec![]),
        ]
    }

    fn sum_of(items: &[i64]) -> i128 {
        items.iter().map(|item| *item as i128).sum()
    }

    /// A `ProvableBigSumIndexedTree` at `[TEST_LEAF, WIDE]` whose entries are
    /// big sum trees, written one call at a time.
    fn build_pbsit(db: &GroveDb, v: &GroveVersion) {
        db.insert(
            [TEST_LEAF].as_ref(),
            WIDE,
            Element::empty_provable_big_sum_indexed_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("create the tree");
        for (key, items) in entries() {
            db.insert_into_provable_big_sum_indexed_tree(
                [TEST_LEAF, WIDE].as_ref(),
                key,
                Element::empty_big_sum_tree(),
                None,
                v,
            )
            .unwrap()
            .expect("insert an entry");
            add_items(db, key, &items, 0, v);
        }
    }

    /// A `ProvableCountProvableBigSumIndexedTree` on the count, avg and big
    /// sum axes whose entries are count and big sum trees.
    fn build_pcpbsit(db: &GroveDb, v: &GroveVersion) {
        db.insert(
            [TEST_LEAF].as_ref(),
            WIDE,
            wide_axes(&[IndexAxis::Count, IndexAxis::Avg, IndexAxis::BigSum]),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("create the tree");
        for (key, items) in entries() {
            db.insert_into_provable_count_provable_big_sum_indexed_tree(
                [TEST_LEAF, WIDE].as_ref(),
                key,
                Element::empty_count_big_sum_tree(),
                None,
                v,
            )
            .unwrap()
            .expect("insert an entry");
            add_items(db, key, &items, 0, v);
        }
    }

    /// Add sum items to the subtree of entry `key`, keyed from `first`.
    fn add_items(db: &GroveDb, key: &[u8], items: &[i64], first: u8, v: &GroveVersion) {
        for (i, item) in items.iter().enumerate() {
            db.insert(
                [TEST_LEAF, WIDE, key].as_ref(),
                &[first + i as u8],
                Element::new_sum_item(*item),
                None,
                None,
                v,
            )
            .unwrap()
            .expect("insert a sum item");
        }
    }

    fn run(db: &GroveDb, path_query: &PathQuery, v: &GroveVersion) -> Result<PathQueryRun, Error> {
        db.run_path_query(
            path_query,
            true,
            true,
            true,
            QueryResultType::QueryKeyElementPairResultType,
            None,
            v,
        )
        .unwrap()
    }

    fn ranking(entries: AxisEntries) -> Vec<(i128, Vec<u8>)> {
        match entries {
            AxisEntries::Count(entries) => entries
                .into_iter()
                .map(|entry| (entry.ordering_value as i128, entry.primary_key))
                .collect(),
            AxisEntries::Sum(entries) => entries
                .into_iter()
                .map(|entry| (entry.ordering_value as i128, entry.primary_key))
                .collect(),
            AxisEntries::Avg(entries) | AxisEntries::BigSum(entries) => entries
                .into_iter()
                .map(|entry| (entry.ordering_value, entry.primary_key))
                .collect(),
        }
    }

    /// The entries `path_query` selects, read and proved: the proof must
    /// verify against the root and name the entries the read returns.
    fn read_and_proved(
        db: &GroveDb,
        path_query: &PathQuery,
        v: &GroveVersion,
    ) -> Vec<(i128, Vec<u8>)> {
        let PathQueryRun::AxisEntries { entries: read, .. } = run(db, path_query, v).expect("read")
        else {
            panic!("expected axis entries");
        };
        let proof = db.prove_query(path_query, None, v).unwrap().expect("prove");
        let VerifiedPathQuery::AxisEntries {
            root_hash, entries, ..
        } = GroveDb::verify_path_query(&proof, path_query, v).expect("verify")
        else {
            panic!("expected proved axis entries");
        };
        assert_eq!(root_hash, db.root_hash(None, v).unwrap().expect("root"));
        assert_eq!(entries, read, "the proof names what the read returns");
        ranking(entries)
    }

    fn top(db: &GroveDb, axis: IndexAxis, v: &GroveVersion) -> Vec<(i128, Vec<u8>)> {
        read_and_proved(db, &PathQuery::new_axis_top_k(path(), axis, 10, 0, true), v)
    }

    fn pairs(expected: &[(i128, &[u8])]) -> Vec<(i128, Vec<u8>)> {
        expected
            .iter()
            .map(|(value, key)| (*value, key.to_vec()))
            .collect()
    }

    fn assert_verifies(db: &GroveDb, v: &GroveVersion) {
        let issues = db
            .verify_grovedb(None, true, true, v)
            .expect("verify_grovedb must not return a hard error");
        assert!(
            issues.is_empty(),
            "verify_grovedb reported issues: {issues:?}"
        );
    }

    fn stored(db: &GroveDb, v: &GroveVersion) -> Element {
        db.get([TEST_LEAF].as_ref(), WIDE, None, v)
            .unwrap()
            .expect("the tree is stored")
    }

    fn allow_non_empty() -> DeleteOptions {
        DeleteOptions {
            allow_deleting_non_empty_trees: true,
            deleting_non_empty_trees_returns_error: false,
            base_root_storage_is_free: true,
            validate_tree_at_path_exists: false,
            backwards_references: BackwardsReferences::DontCheck,
        }
    }

    #[test]
    fn big_sum_axis_ranks_entries_by_sums_past_i64() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        build_pbsit(&db, v);

        assert_eq!(
            top(&db, IndexAxis::BigSum, v),
            pairs(&[
                (3 * MAX, b"b"),
                (2 * MAX, b"a"),
                (5, b"c"),
                (0, b"e"),
                (2 * MIN, b"d"),
            ]),
        );

        // The tree records the sum of its entries at full width.
        let total: i128 = entries().iter().map(|(_, items)| sum_of(items)).sum();
        assert!(total > MAX, "the total is past an i64");
        match stored(&db, v) {
            Element::ProvableBigSumIndexedTree(_, _, sum, _) => assert_eq!(sum, total),
            other => panic!("expected a ProvableBigSumIndexedTree, got {other:?}"),
        }
        assert_verifies(&db, v);
    }

    #[test]
    fn big_sum_axis_pages_and_reads_keys_only() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        build_pbsit(&db, v);

        // The second page of two, lowest first.
        assert_eq!(
            read_and_proved(
                &db,
                &PathQuery::new_axis_top_k(path(), IndexAxis::BigSum, 2, 2, false),
                v
            ),
            pairs(&[(5, b"c"), (2 * MAX, b"a")]),
        );

        // The keys projection returns the same ranking pairs.
        let keys_query = PathQuery::new_axis(
            path(),
            AxisQuery::top_k(IndexAxis::BigSum, 2, 0, true).keys_only(),
        );
        let PathQueryRun::AxisKeys { keys, skipped } = run(&db, &keys_query, v).expect("read")
        else {
            panic!("expected axis keys");
        };
        assert_eq!(skipped, Some(0));
        assert_eq!(
            keys,
            AxisKeys::BigSum(pairs(&[(3 * MAX, b"b"), (2 * MAX, b"a")]))
        );
    }

    #[test]
    fn big_sum_axis_bounds_are_128_bits_wide() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        build_pbsit(&db, v);

        // A band that lies wholly past i64::MAX.
        let above =
            PathQuery::new_axis_bounded(path(), IndexAxis::BigSum, MAX + 1, i128::MAX, 10, false);
        assert_eq!(
            read_and_proved(&db, &above, v),
            pairs(&[(2 * MAX, b"a"), (3 * MAX, b"b")]),
        );

        // A band that lies wholly below i64::MIN.
        let below =
            PathQuery::new_axis_bounded(path(), IndexAxis::BigSum, i128::MIN, MIN - 1, 10, true);
        assert_eq!(read_and_proved(&db, &below, v), pairs(&[(2 * MIN, b"d")]));

        // The population of a band is read and proved.
        let population = PathQuery::new_axis_aggregate_over_value_range(
            path(),
            IndexAxis::BigSum,
            0,
            2 * MAX,
            AggregateFold::Population,
        );
        let PathQueryRun::AxisAggregate(AxisAggregateValue::Population(read)) =
            run(&db, &population, v).expect("read")
        else {
            panic!("expected a population");
        };
        assert_eq!(read, 3, "e (0), c (5) and a (2 * MAX)");
        let proof = db
            .prove_query(&population, None, v)
            .unwrap()
            .expect("prove");
        let VerifiedPathQuery::AxisAggregate { root_hash, value } =
            GroveDb::verify_path_query(&proof, &population, v).expect("verify")
        else {
            panic!("expected a proved aggregate");
        };
        assert_eq!(root_hash, db.root_hash(None, v).unwrap().expect("root"));
        assert_eq!(value, 3);

        // The rank of one key.
        let rank = PathQuery::new_axis_rank_of_key(path(), IndexAxis::BigSum, b"c".to_vec(), true);
        let PathQueryRun::AxisRank(read) = run(&db, &rank, v).expect("read") else {
            panic!("expected a rank");
        };
        assert_eq!(read, 2, "b and a rank above c");
        let proof = db.prove_query(&rank, None, v).unwrap().expect("prove");
        let VerifiedPathQuery::AxisRank { root_hash, rank } =
            GroveDb::verify_path_query(&proof, &rank, v).expect("verify")
        else {
            panic!("expected a proved rank");
        };
        assert_eq!(root_hash, db.root_hash(None, v).unwrap().expect("root"));
        assert_eq!(rank, 2);
    }

    #[test]
    fn big_sum_axis_has_no_value_range_total() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        build_pbsit(&db, v);

        let total = PathQuery::new_axis_aggregate_over_value_range(
            path(),
            IndexAxis::BigSum,
            0,
            2 * MAX,
            AggregateFold::Total,
        );
        assert!(
            AxisQuery::aggregate_over_value_range(IndexAxis::BigSum, 0, 1, AggregateFold::Total)
                .validate()
                .is_err(),
            "the query is refused where it is validated"
        );
        assert!(run(&db, &total, v).is_err(), "the read refuses the total");
        assert!(
            db.prove_query(&total, None, v).unwrap().is_err(),
            "the prover refuses the total"
        );
    }

    #[test]
    fn mirror_follows_updates_and_removals() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        build_pbsit(&db, v);

        // c grows past b: its row moves to the top.
        add_items(&db, b"c", &[i64::MAX, i64::MAX, i64::MAX], 10, v);
        let c = 5 + 3 * MAX;
        assert_eq!(
            top(&db, IndexAxis::BigSum, v),
            pairs(&[
                (c, b"c"),
                (3 * MAX, b"b"),
                (2 * MAX, b"a"),
                (0, b"e"),
                (2 * MIN, b"d"),
            ]),
        );
        assert_verifies(&db, v);

        // d is removed with its subtree: its row goes with it.
        let removed = db
            .delete_from_provable_big_sum_indexed_tree([TEST_LEAF, WIDE].as_ref(), b"d", None, v)
            .unwrap()
            .expect("delete an entry");
        assert!(removed);
        assert_eq!(
            top(&db, IndexAxis::BigSum, v),
            pairs(&[(c, b"c"), (3 * MAX, b"b"), (2 * MAX, b"a"), (0, b"e")]),
        );
        match stored(&db, v) {
            Element::ProvableBigSumIndexedTree(_, _, sum, _) => {
                assert_eq!(sum, c + 3 * MAX + 2 * MAX)
            }
            other => panic!("expected a ProvableBigSumIndexedTree, got {other:?}"),
        }
        assert_verifies(&db, v);
    }

    #[test]
    fn count_and_big_sum_tree_ranks_on_each_of_its_axes() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        build_pcpbsit(&db, v);
        // A bare sum item is an entry too: it counts one and adds its sum.
        db.insert_into_provable_count_provable_big_sum_indexed_tree(
            [TEST_LEAF, WIDE].as_ref(),
            b"f",
            Element::new_sum_item(7),
            None,
            v,
        )
        .unwrap()
        .expect("insert a sum item entry");

        assert_eq!(
            top(&db, IndexAxis::BigSum, v),
            pairs(&[
                (3 * MAX, b"b"),
                (2 * MAX, b"a"),
                (7, b"f"),
                (5, b"c"),
                (0, b"e"),
                (2 * MIN, b"d"),
            ]),
        );
        // By count, ties broken by key, highest first.
        assert_eq!(
            top(&db, IndexAxis::Count, v),
            pairs(&[
                (3, b"b"),
                (2, b"d"),
                (2, b"a"),
                (1, b"f"),
                (1, b"c"),
                (0, b"e"),
            ]),
        );
        // By average: the 128-bit sum over the count, in fixed point.
        let avg = |sum: i128, count: u64| compute_big_avg_fixed_point(sum, count);
        assert_eq!(
            top(&db, IndexAxis::Avg, v),
            pairs(&[
                (avg(3 * MAX, 3), b"b"),
                (avg(2 * MAX, 2), b"a"),
                (avg(7, 1), b"f"),
                (avg(5, 1), b"c"),
                (avg(0, 0), b"e"),
                (avg(2 * MIN, 2), b"d"),
            ]),
        );

        let (count, total): (u64, i128) = entries()
            .iter()
            .map(|(_, items)| (items.len() as u64, sum_of(items)))
            .fold((1, 7), |(count, sum), (c, s)| (count + c, sum + s));
        match stored(&db, v) {
            Element::ProvableCountProvableBigSumIndexedTree(_, stored_count, sum, ..) => {
                assert_eq!((stored_count, sum), (count, total))
            }
            other => panic!("expected a ProvableCountProvableBigSumIndexedTree, got {other:?}"),
        }
        assert_verifies(&db, v);

        let removed = db
            .delete_from_provable_count_provable_big_sum_indexed_tree(
                [TEST_LEAF, WIDE].as_ref(),
                b"b",
                None,
                v,
            )
            .unwrap()
            .expect("delete an entry");
        assert!(removed);
        for axis in [IndexAxis::Count, IndexAxis::Avg, IndexAxis::BigSum] {
            assert!(
                top(&db, axis, v).iter().all(|(_, key)| key != b"b"),
                "{axis:?}: the removed entry has no row"
            );
        }
        assert_verifies(&db, v);
    }

    /// The same writes in the same order, each as a batch of one operation,
    /// commit the root the direct calls commit. (One batch of all of them
    /// would hold the same entries in a tree of another shape, since a Merk's
    /// shape follows the order its keys arrive in.)
    #[test]
    fn a_batch_commits_the_root_direct_writes_commit() {
        let v = GroveVersion::latest();
        let direct = make_test_grovedb(v);
        build_pcpbsit(&direct, v);

        let batched = make_test_grovedb(v);
        let mut ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            WIDE.to_vec(),
            wide_axes(&[IndexAxis::Count, IndexAxis::Avg, IndexAxis::BigSum]),
        )];
        for (key, items) in entries() {
            ops.push(QualifiedGroveDbOp::insert_or_replace_op(
                path(),
                key.to_vec(),
                Element::empty_count_big_sum_tree(),
            ));
            for (i, item) in items.iter().enumerate() {
                ops.push(QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), WIDE.to_vec(), key.to_vec()],
                    vec![i as u8],
                    Element::new_sum_item(*item),
                ));
            }
        }
        for op in ops {
            batched
                .apply_batch(vec![op], None, None, v)
                .unwrap()
                .expect("apply one operation");
        }

        assert_eq!(
            batched.root_hash(None, v).unwrap().expect("root"),
            direct.root_hash(None, v).unwrap().expect("root"),
        );
        assert_verifies(&batched, v);
    }

    /// One batch that creates the tree, its entries and their contents
    /// leaves the rows a reader ranks by, on every axis.
    #[test]
    fn one_batch_builds_the_tree_and_its_rows() {
        let v = GroveVersion::latest();
        let direct = make_test_grovedb(v);
        build_pcpbsit(&direct, v);

        let batched = make_test_grovedb(v);
        let mut ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            WIDE.to_vec(),
            wide_axes(&[IndexAxis::Count, IndexAxis::Avg, IndexAxis::BigSum]),
        )];
        for (key, items) in entries() {
            ops.push(QualifiedGroveDbOp::insert_or_replace_op(
                path(),
                key.to_vec(),
                Element::empty_count_big_sum_tree(),
            ));
            for (i, item) in items.iter().enumerate() {
                ops.push(QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), WIDE.to_vec(), key.to_vec()],
                    vec![i as u8],
                    Element::new_sum_item(*item),
                ));
            }
        }
        batched
            .apply_batch(ops, None, None, v)
            .unwrap()
            .expect("apply the batch");

        for axis in [IndexAxis::Count, IndexAxis::Avg, IndexAxis::BigSum] {
            assert_eq!(top(&batched, axis, v), top(&direct, axis, v), "{axis:?}");
        }
        assert_verifies(&batched, v);
    }

    #[test]
    fn a_removed_tree_leaves_no_rows_behind() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        build_pbsit(&db, v);
        let remove = |db: &GroveDb| {
            db.delete([TEST_LEAF].as_ref(), WIDE, Some(allow_non_empty()), None, v)
                .unwrap()
                .expect("delete the tree");
        };

        // The same tree again at the same path: a row left behind would
        // rank beside the one new entry.
        remove(&db);
        db.insert(
            [TEST_LEAF].as_ref(),
            WIDE,
            Element::empty_provable_big_sum_indexed_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("create the tree again");
        db.insert_into_provable_big_sum_indexed_tree(
            [TEST_LEAF, WIDE].as_ref(),
            b"y",
            Element::new_sum_item(4),
            None,
            v,
        )
        .unwrap()
        .expect("insert an entry");
        assert_eq!(top(&db, IndexAxis::BigSum, v), pairs(&[(4, b"y")]));
        assert_verifies(&db, v);

        // And the 64-bit tree at that path: its sum axis lives in the
        // storage namespace the big sum axis used.
        remove(&db);
        db.insert(
            [TEST_LEAF].as_ref(),
            WIDE,
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("create the 64-bit tree at the same path");
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, WIDE].as_ref(),
            b"z",
            Element::new_sum_item(9),
            None,
            v,
        )
        .unwrap()
        .expect("insert an entry");
        assert_eq!(top(&db, IndexAxis::Sum, v), pairs(&[(9, b"z")]));
        assert_verifies(&db, v);
    }

    /// `[TEST_LEAF] / WIDE / *`: an ordinary query that descends through the
    /// indexed tree into its primary.
    fn entries_subquery() -> PathQuery {
        let mut inner = grovedb_merk::proofs::Query::new();
        inner.insert_all();
        let mut outer = grovedb_merk::proofs::Query::new();
        outer.insert_key(WIDE.to_vec());
        outer.set_subquery(inner);
        PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], outer)
    }

    /// `[TEST_LEAF] / WIDE`: the indexed tree's own element.
    fn tree_query() -> PathQuery {
        let mut query = grovedb_merk::proofs::Query::new();
        query.insert_key(WIDE.to_vec());
        PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query)
    }

    fn proved_elements(db: &GroveDb, path_query: &PathQuery, v: &GroveVersion) -> usize {
        let proof = db.prove_query(path_query, None, v).unwrap().expect("prove");
        let (root_hash, results) = GroveDb::verify_query(&proof, path_query, v).expect("verify");
        assert_eq!(root_hash, db.root_hash(None, v).unwrap().expect("root"));
        results.len()
    }

    #[test]
    fn an_ordinary_query_descends_through_either_tree() {
        let v = GroveVersion::latest();
        for build in [build_pbsit, build_pcpbsit] {
            let db = make_test_grovedb(v);
            build(&db, v);
            assert_eq!(
                proved_elements(&db, &entries_subquery(), v),
                entries().len(),
                "the subquery proves every entry"
            );
            assert_eq!(proved_elements(&db, &tree_query(), v), 1);

            // The unproved read returns the same entries.
            let (read, _) = db
                .query_raw(
                    &entries_subquery(),
                    true,
                    true,
                    true,
                    QueryResultType::QueryKeyElementPairResultType,
                    None,
                    v,
                )
                .unwrap()
                .expect("query");
            assert_eq!(read.len(), entries().len());
        }
    }

    /// An indexed tree is an entry of another: the outer tree ranks the
    /// inner one by the count and the 128-bit sum the inner one records.
    #[test]
    fn an_indexed_tree_is_an_entry_of_another() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            WIDE,
            wide_axes(&[IndexAxis::Count, IndexAxis::BigSum]),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("create the outer tree");
        // A 128-bit inner tree, a 64-bit inner tree, and a plain entry.
        db.insert_into_provable_count_provable_big_sum_indexed_tree(
            [TEST_LEAF, WIDE].as_ref(),
            b"wide_inner",
            wide_axes(&[IndexAxis::BigSum]),
            None,
            v,
        )
        .unwrap()
        .expect("insert a 128-bit inner tree");
        db.insert_into_provable_count_provable_big_sum_indexed_tree(
            [TEST_LEAF, WIDE].as_ref(),
            b"narrow_inner",
            Element::empty_provable_count_provable_sum_indexed_tree(vec![(
                IndexAxis::Sum.tag(),
                None,
            )])
            .expect("canonical axes"),
            None,
            v,
        )
        .unwrap()
        .expect("insert a 64-bit inner tree");
        db.insert_into_provable_count_provable_big_sum_indexed_tree(
            [TEST_LEAF, WIDE].as_ref(),
            b"plain",
            Element::new_sum_item(11),
            None,
            v,
        )
        .unwrap()
        .expect("insert a plain entry");

        for (i, group) in [b"x".as_slice(), b"y", b"z"].into_iter().enumerate() {
            db.insert_into_provable_count_provable_big_sum_indexed_tree(
                [TEST_LEAF, WIDE, b"wide_inner".as_slice()].as_ref(),
                group,
                Element::empty_count_big_sum_tree(),
                None,
                v,
            )
            .unwrap()
            .expect("insert an entry of the inner tree");
            for item in 0..=i as u8 {
                db.insert(
                    [TEST_LEAF, WIDE, b"wide_inner".as_slice(), group].as_ref(),
                    &[item],
                    Element::new_sum_item(i64::MAX),
                    None,
                    None,
                    v,
                )
                .unwrap()
                .expect("insert a sum item");
            }
        }
        for (key, sum) in [(b"p".as_slice(), 3i64), (b"q", 4)] {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, WIDE, b"narrow_inner".as_slice()].as_ref(),
                key,
                Element::new_sum_item(sum),
                None,
                v,
            )
            .unwrap()
            .expect("insert an entry of the 64-bit inner tree");
        }

        // The inner tree: x holds 1 item, y 2, z 3, of i64::MAX each.
        let inner_path = vec![TEST_LEAF.to_vec(), WIDE.to_vec(), b"wide_inner".to_vec()];
        assert_eq!(
            read_and_proved(
                &db,
                &PathQuery::new_axis_top_k(inner_path, IndexAxis::BigSum, 10, 0, true),
                v
            ),
            pairs(&[(3 * MAX, b"z"), (2 * MAX, b"y"), (MAX, b"x")]),
        );
        // The outer tree ranks the inner trees by what they record.
        assert_eq!(
            top(&db, IndexAxis::BigSum, v),
            pairs(&[
                (6 * MAX, b"wide_inner"),
                (11, b"plain"),
                (7, b"narrow_inner")
            ]),
        );
        assert_eq!(
            top(&db, IndexAxis::Count, v),
            pairs(&[(6, b"wide_inner"), (2, b"narrow_inner"), (1, b"plain")]),
        );
        assert_verifies(&db, v);
    }

    /// An empty tree is proved as its 64-bit counterpart is: the element
    /// itself, and nothing below it.
    #[test]
    fn an_empty_tree_is_proved_as_its_64_bit_counterpart_is() {
        let v = GroveVersion::latest();
        let proved = |element: Element| {
            let db = make_test_grovedb(v);
            db.insert([TEST_LEAF].as_ref(), WIDE, element, None, None, v)
                .unwrap()
                .expect("create the tree");
            assert_verifies(&db, v);
            (
                proved_elements(&db, &tree_query(), v),
                proved_elements(&db, &entries_subquery(), v),
            )
        };
        let narrow = proved(Element::empty_provable_sum_indexed_tree());
        assert_eq!(narrow.0, 1, "the element itself is proved");

        for element in [
            Element::empty_provable_big_sum_indexed_tree(),
            wide_axes(&[IndexAxis::Count, IndexAxis::Avg, IndexAxis::BigSum]),
            wide_axes(&[IndexAxis::BigSum]),
        ] {
            assert_eq!(proved(element), narrow);
        }

        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            WIDE,
            Element::empty_provable_big_sum_indexed_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("create the tree");
        assert_eq!(top(&db, IndexAxis::BigSum, v), Vec::new());
    }

    #[test]
    fn reconcile_rebuilds_the_rows_it_already_has() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        build_pcpbsit(&db, v);
        let before = db.root_hash(None, v).unwrap().expect("root");

        db.reconcile_indexed_tree_secondaries([TEST_LEAF, WIDE].as_ref(), None, v)
            .unwrap()
            .expect("reconcile");

        assert_eq!(db.root_hash(None, v).unwrap().expect("root"), before);
        assert_verifies(&db, v);
    }

    #[test]
    fn each_width_has_its_own_sum_axis() {
        let v = GroveVersion::latest();

        // The axis lists.
        assert!(
            Element::empty_provable_count_provable_big_sum_indexed_tree(vec![(
                IndexAxis::Sum.tag(),
                None
            )])
            .is_err(),
            "a 128-bit tree has no sum axis"
        );
        assert!(
            Element::empty_provable_count_provable_sum_indexed_tree(vec![(
                IndexAxis::BigSum.tag(),
                None
            )])
            .is_err(),
            "a 64-bit tree has no big sum axis"
        );

        // An element built past the constructors is refused on the way in.
        let db = make_test_grovedb(v);
        for (key, element) in [
            (
                b"wide_with_sum".as_slice(),
                Element::ProvableCountProvableBigSumIndexedTree(
                    None,
                    0,
                    0,
                    vec![(IndexAxis::Sum.tag(), None)],
                    None,
                ),
            ),
            (
                b"narrow_with_big_sum".as_slice(),
                Element::ProvableCountProvableSumIndexedTree(
                    None,
                    0,
                    0,
                    vec![(IndexAxis::BigSum.tag(), None)],
                    None,
                ),
            ),
        ] {
            assert!(
                db.insert([TEST_LEAF].as_ref(), key, element.clone(), None, None, v)
                    .unwrap()
                    .is_err(),
                "{key:?}: a direct insert refuses the axis"
            );
            assert!(
                db.apply_batch(
                    vec![QualifiedGroveDbOp::insert_or_replace_op(
                        vec![TEST_LEAF.to_vec()],
                        key.to_vec(),
                        element,
                    )],
                    None,
                    None,
                    v,
                )
                .unwrap()
                .is_err(),
                "{key:?}: a batch refuses the axis"
            );
        }

        // A read names the axis the tree has.
        build_pbsit(&db, v);
        assert!(
            run(
                &db,
                &PathQuery::new_axis_top_k(path(), IndexAxis::Sum, 5, 0, true),
                v
            )
            .is_err(),
            "the sum axis is not indexed on a 128-bit tree"
        );
        db.insert(
            [TEST_LEAF].as_ref(),
            b"narrow",
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("create a 64-bit tree");
        assert!(
            run(
                &db,
                &PathQuery::new_axis_top_k(
                    vec![TEST_LEAF.to_vec(), b"narrow".to_vec()],
                    IndexAxis::BigSum,
                    5,
                    0,
                    true
                ),
                v
            )
            .is_err(),
            "the big sum axis is not indexed on a 64-bit tree"
        );
    }

    #[test]
    fn children_must_carry_what_the_tree_indexes() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"sum_only",
            Element::empty_provable_big_sum_indexed_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("create the tree");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"count_and_sum",
            wide_axes(&[IndexAxis::Count, IndexAxis::BigSum]),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("create the tree");

        // An item carries no sum.
        assert!(db
            .insert_into_provable_big_sum_indexed_tree(
                [TEST_LEAF, b"sum_only".as_slice()].as_ref(),
                b"k",
                Element::new_item(b"v".to_vec()),
                None,
                v,
            )
            .unwrap()
            .is_err());
        // A big sum tree carries a sum and no count of its contents.
        assert!(db
            .insert_into_provable_count_provable_big_sum_indexed_tree(
                [TEST_LEAF, b"count_and_sum".as_slice()].as_ref(),
                b"k",
                Element::empty_big_sum_tree(),
                None,
                v,
            )
            .unwrap()
            .is_err());
        // The trees of either width that carry both are accepted.
        for (key, child) in [
            (b"narrow".as_slice(), Element::empty_count_sum_tree()),
            (b"wide".as_slice(), Element::empty_count_big_sum_tree()),
            (
                b"provable".as_slice(),
                Element::empty_provable_count_provable_big_sum_tree(),
            ),
        ] {
            db.insert_into_provable_count_provable_big_sum_indexed_tree(
                [TEST_LEAF, b"count_and_sum".as_slice()].as_ref(),
                key,
                child,
                None,
                v,
            )
            .unwrap()
            .expect("a count and sum child is accepted");
        }
        // A generic write cannot mirror, so it is refused as for every
        // indexed tree.
        assert!(db
            .insert(
                [TEST_LEAF, b"sum_only".as_slice()].as_ref(),
                b"k",
                Element::new_sum_item(1),
                None,
                None,
                v,
            )
            .unwrap()
            .is_err());
        assert_verifies(&db, v);
    }

    /// The big sum axis prepends a 16-byte sort key, so an item key is
    /// capped at 239 bytes, as under the avg axis.
    #[test]
    fn item_keys_leave_room_for_the_16_byte_sort_key() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            WIDE,
            Element::empty_provable_big_sum_indexed_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("create the tree");

        let insert = |key: &[u8]| {
            db.insert_into_provable_big_sum_indexed_tree(
                [TEST_LEAF, WIDE].as_ref(),
                key,
                Element::new_sum_item(1),
                None,
                v,
            )
            .unwrap()
        };
        insert(&[b'k'; 239]).expect("a 239-byte key fits");
        assert!(insert(&[b'k'; 240]).is_err(), "a 240-byte key does not");
        assert_eq!(top(&db, IndexAxis::BigSum, v), vec![(1, vec![b'k'; 239])]);
        assert_verifies(&db, v);
    }

    #[test]
    fn creation_is_gated_to_the_version_that_introduces_the_trees() {
        use grovedb_version::version::{v1::GROVE_V1, v2::GROVE_V2, v3::GROVE_V3, v4::GROVE_V4};

        let is_version_refusal = |result: &Result<(), Error>| {
            matches!(
                result,
                Err(Error::VersionError(
                    grovedb_version::error::GroveVersionError::UnknownVersionMismatch { .. }
                ))
            )
        };

        for grove_version in [&GROVE_V1, &GROVE_V2, &GROVE_V3, &GROVE_V4] {
            let db = make_test_grovedb(grove_version);
            for (key, element) in [
                (
                    b"single".as_slice(),
                    Element::empty_provable_big_sum_indexed_tree(),
                ),
                (
                    b"multi".as_slice(),
                    wide_axes(&[IndexAxis::Count, IndexAxis::BigSum]),
                ),
            ] {
                let direct = db
                    .insert(
                        [TEST_LEAF].as_ref(),
                        key,
                        element.clone(),
                        None,
                        None,
                        grove_version,
                    )
                    .unwrap();
                assert!(
                    is_version_refusal(&direct),
                    "{key:?}: a direct insert is refused, got {direct:?}"
                );
                let batched = db
                    .apply_batch(
                        vec![QualifiedGroveDbOp::insert_or_replace_op(
                            vec![TEST_LEAF.to_vec()],
                            key.to_vec(),
                            element,
                        )],
                        None,
                        None,
                        grove_version,
                    )
                    .unwrap();
                assert!(
                    is_version_refusal(&batched),
                    "{key:?}: a batch is refused, got {batched:?}"
                );
            }
        }
    }
}
