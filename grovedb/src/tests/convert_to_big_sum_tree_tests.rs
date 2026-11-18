//! `GroveDb::convert_to_big_sum_tree`: a tree that keeps a 64-bit sum becomes
//! its counterpart with a 128-bit sum, in place.
//!
//! The measure of a conversion is the tree built wide from the start: the
//! four plain trees must end with the element and the root hash that tree
//! has, and the two indexed trees with the rankings it has. After it the
//! tree takes sums past the `i64` range, and the conversions that would
//! change what a parent sums are refused.

#[cfg(test)]
mod tests {
    use grovedb_element::indexed::IndexAxis;
    use grovedb_merk::proofs::Query;
    use grovedb_version::version::GroveVersion;

    use crate::{
        operations::{
            get::PathQueryRun,
            proof::{indexed_axis::AxisEntries, VerifiedPathQuery},
        },
        query_result_type::QueryResultType,
        tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
        Element, Error, GroveDb, PathQuery,
    };

    const TREE: &[u8] = b"tree";
    const MAX: i128 = i64::MAX as i128;

    fn insert(db: &GroveDb, path: &[&[u8]], key: &[u8], element: Element, v: &GroveVersion) {
        db.insert(path, key, element, None, None, v)
            .unwrap()
            .expect("insert");
    }

    /// The element as stored, inside its wrapper.
    fn stored(db: &GroveDb, path: &[&[u8]], key: &[u8], v: &GroveVersion) -> Element {
        db.get_raw(path.into(), key, None, v)
            .unwrap()
            .expect("stored")
    }

    fn root(db: &GroveDb, v: &GroveVersion) -> [u8; 32] {
        db.root_hash(None, v).unwrap().expect("root hash")
    }

    fn convert(db: &GroveDb, path: &[&[u8]], key: &[u8], v: &GroveVersion) -> Result<(), Error> {
        db.convert_to_big_sum_tree(path, key, None, v).unwrap()
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

    /// The five trees without a ranking: `(name, the tree, its counterpart)`.
    /// A `ProvableCountSumTree` and a `ProvableCountProvableSumTree` have the
    /// same counterpart: no tree with a 128-bit sum has its count in its
    /// node hashes and its sum beside them.
    fn plain_trees() -> Vec<(&'static str, Element, Element)> {
        vec![
            (
                "SumTree",
                Element::empty_sum_tree(),
                Element::empty_big_sum_tree(),
            ),
            (
                "CountSumTree",
                Element::empty_count_sum_tree(),
                Element::empty_count_big_sum_tree(),
            ),
            (
                "ProvableSumTree",
                Element::empty_provable_sum_tree(),
                Element::empty_provable_big_sum_tree(),
            ),
            (
                "ProvableCountSumTree",
                Element::empty_provable_count_sum_tree(),
                Element::empty_provable_count_provable_big_sum_tree(),
            ),
            (
                "ProvableCountProvableSumTree",
                Element::empty_provable_count_provable_sum_tree(),
                Element::empty_provable_count_provable_big_sum_tree(),
            ),
        ]
    }

    /// Children of every kind a sum tree holds, enough of them for a Merk
    /// several levels deep: sum items of both signs, an item, an item with a
    /// sum, and a sum tree with items of its own.
    fn fill(db: &GroveDb, v: &GroveVersion) {
        let tree: &[&[u8]] = &[TEST_LEAF, TREE];
        for i in 0..20u8 {
            insert(
                db,
                tree,
                &[b'k', i],
                Element::new_sum_item(i64::from(i) * 1_000 - 5_000),
                v,
            );
        }
        insert(db, tree, b"item", Element::new_item(b"payload".to_vec()), v);
        insert(
            db,
            tree,
            b"item_with_sum",
            Element::new_item_with_sum_item(b"payload".to_vec(), 77),
            v,
        );
        insert(db, tree, b"nested", Element::empty_sum_tree(), v);
        for i in 0..3u8 {
            insert(
                db,
                &[TEST_LEAF, TREE, b"nested"],
                &[i],
                Element::new_sum_item(i64::from(i) + 1),
                v,
            );
        }
    }

    /// The sum `fill` leaves: the 20 sum items, the item with a sum and the
    /// nested tree's three items.
    const FILLED_SUM: i128 = 90_000 + 77 + 6;

    /// Two more children, each the largest sum item there is.
    fn add_past_i64(db: &GroveDb, v: &GroveVersion) {
        for key in [b"max1", b"max2"] {
            insert(
                db,
                &[TEST_LEAF, TREE],
                key,
                Element::new_sum_item(i64::MAX),
                v,
            );
        }
    }

    fn built(tree: &Element, v: &GroveVersion) -> TempGroveDb {
        let db = make_test_grovedb(v);
        insert(&db, &[TEST_LEAF], TREE, tree.clone(), v);
        fill(&db, v);
        db
    }

    fn sum_of(element: &Element) -> i128 {
        match element.underlying() {
            Element::BigSumTree(_, sum, _)
            | Element::ProvableBigSumTree(_, sum, _)
            | Element::CountBigSumTree(_, _, sum, _)
            | Element::ProvableCountProvableBigSumTree(_, _, sum, _)
            | Element::ProvableBigSumIndexedTree(_, _, sum, _)
            | Element::ProvableCountProvableBigSumIndexedTree(_, _, sum, ..) => *sum,
            other => panic!("not a tree with a 128-bit sum: {other:?}"),
        }
    }

    /// Every child of the tree, proved: the proof must verify against the
    /// database's root.
    fn proved_children(db: &GroveDb, v: &GroveVersion) -> usize {
        let path_query = PathQuery::new_unsized(
            vec![TEST_LEAF.to_vec(), TREE.to_vec()],
            Query::new_range_full(),
        );
        let proof = db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove");
        let (root_hash, children) = GroveDb::verify_query(&proof, &path_query, v).expect("verify");
        assert_eq!(root_hash, root(db, v));
        children.len()
    }

    #[test]
    fn a_converted_tree_is_the_tree_built_wide() {
        let v = GroveVersion::latest();
        for (name, narrow, wide) in plain_trees() {
            let converted = built(&narrow, v);
            convert(&converted, &[TEST_LEAF], TREE, v).expect(name);
            let built_wide = built(&wide, v);

            let element = stored(&converted, &[TEST_LEAF], TREE, v);
            assert_eq!(
                element,
                stored(&built_wide, &[TEST_LEAF], TREE, v),
                "{name}"
            );
            assert_eq!(sum_of(&element), FILLED_SUM, "{name}");
            assert_eq!(root(&converted, v), root(&built_wide, v), "{name}");
            assert_verifies(&converted, v);
            assert_eq!(proved_children(&converted, v), 23, "{name}");

            // The two stay the same tree under the writes that follow, and
            // the sum passes the i64 range.
            add_past_i64(&converted, v);
            add_past_i64(&built_wide, v);
            let element = stored(&converted, &[TEST_LEAF], TREE, v);
            assert_eq!(sum_of(&element), FILLED_SUM + 2 * MAX, "{name}");
            assert_eq!(root(&converted, v), root(&built_wide, v), "{name}");
            assert_verifies(&converted, v);
            assert_eq!(proved_children(&converted, v), 25, "{name}");
        }
    }

    #[test]
    fn the_tree_overflows_until_it_is_converted() {
        let v = GroveVersion::latest();
        for (name, narrow, _) in plain_trees() {
            let db = built(&narrow, v);
            let overflowing = db
                .insert(
                    [TEST_LEAF, TREE].as_ref(),
                    b"max1",
                    Element::new_sum_item(i64::MAX),
                    None,
                    None,
                    v,
                )
                .unwrap();
            assert!(overflowing.is_err(), "{name}: {overflowing:?}");

            convert(&db, &[TEST_LEAF], TREE, v).expect(name);
            add_past_i64(&db, v);
            assert_eq!(
                sum_of(&stored(&db, &[TEST_LEAF], TREE, v)),
                FILLED_SUM + 2 * MAX,
                "{name}"
            );
        }
    }

    #[test]
    fn an_empty_tree_and_a_root_leaf_are_converted() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);

        insert(&db, &[TEST_LEAF], TREE, Element::empty_sum_tree(), v);
        convert(&db, &[TEST_LEAF], TREE, v).expect("an empty tree");
        assert_eq!(
            stored(&db, &[TEST_LEAF], TREE, v),
            Element::empty_big_sum_tree()
        );

        insert(&db, &[], b"leaf", Element::empty_count_sum_tree(), v);
        insert(&db, &[b"leaf"], b"a", Element::new_sum_item(4), v);
        convert(&db, &[], b"leaf", v).expect("a root leaf");
        assert_eq!(
            stored(&db, &[], b"leaf", v),
            Element::CountBigSumTree(Some(b"a".to_vec()), 1, 4, None)
        );
        assert_verifies(&db, v);
    }

    #[test]
    fn the_flags_and_the_wrapper_are_kept() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        let flags = Some(vec![7, 7, 7]);

        insert(
            &db,
            &[TEST_LEAF],
            TREE,
            Element::empty_sum_tree_with_flags(flags.clone()),
            v,
        );
        insert(&db, &[TEST_LEAF, TREE], b"a", Element::new_sum_item(4), v);
        convert(&db, &[TEST_LEAF], TREE, v).expect("a tree with flags");
        assert_eq!(
            stored(&db, &[TEST_LEAF], TREE, v),
            Element::BigSumTree(Some(b"a".to_vec()), 4, flags)
        );

        // A tree its count tree parent does not count stays out of the count.
        insert(
            &db,
            &[TEST_LEAF],
            b"counted",
            Element::empty_count_tree(),
            v,
        );
        insert(
            &db,
            &[TEST_LEAF, b"counted"],
            b"one",
            Element::new_item(vec![1]),
            v,
        );
        insert(
            &db,
            &[TEST_LEAF, b"counted"],
            TREE,
            Element::new_non_counted(Element::empty_sum_tree()).expect("wrap"),
            v,
        );
        convert(&db, &[TEST_LEAF, b"counted"], TREE, v).expect("a wrapped tree");
        assert_eq!(
            stored(&db, &[TEST_LEAF, b"counted"], TREE, v),
            Element::new_non_counted(Element::empty_big_sum_tree()).expect("wrap")
        );
        assert!(matches!(
            stored(&db, &[TEST_LEAF], b"counted", v),
            Element::CountTree(_, 1, _)
        ));
        assert_verifies(&db, v);
    }

    #[test]
    fn the_conversion_is_part_of_its_transaction() {
        let v = GroveVersion::latest();
        let db = built(&Element::empty_provable_sum_tree(), v);
        let before = root(&db, v);

        let tx = db.start_transaction();
        db.convert_to_big_sum_tree([TEST_LEAF].as_ref(), TREE, Some(&tx), v)
            .unwrap()
            .expect("convert in a transaction");
        assert!(matches!(
            stored(&db, &[TEST_LEAF], TREE, v),
            Element::ProvableSumTree(..)
        ));
        assert_eq!(root(&db, v), before);

        db.commit_transaction(tx).unwrap().expect("commit");
        assert!(matches!(
            stored(&db, &[TEST_LEAF], TREE, v),
            Element::ProvableBigSumTree(..)
        ));
        assert_ne!(root(&db, v), before);
    }

    #[test]
    fn the_cost_reports_each_node_and_the_element_as_grown() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        insert(&db, &[TEST_LEAF], TREE, Element::empty_sum_tree(), v);
        for i in 0..5u8 {
            insert(
                &db,
                &[TEST_LEAF, TREE],
                &[i],
                Element::new_sum_item(i64::from(i)),
                v,
            );
        }

        let conversion = db.convert_to_big_sum_tree([TEST_LEAF].as_ref(), TREE, None, v);
        conversion.value.expect("convert");
        let storage = conversion.cost.storage_cost;

        // A child grows by the 8 bytes of its own sum and the 8 of the sum
        // on the link to it, and the element by the 7 between the cost
        // sizes of the two trees (`SUM_TREE_COST_SIZE`,
        // `BIG_SUM_TREE_COST_SIZE`).
        assert_eq!(storage.added_bytes, 5 * 16 + 7);
        assert_eq!(storage.removed_bytes.total_removed_bytes(), 0);
    }

    // ---- the indexed trees ----

    fn narrow_axes(axes: &[IndexAxis]) -> Element {
        Element::empty_provable_count_provable_sum_indexed_tree(
            axes.iter().map(|axis| (axis.tag(), None)).collect(),
        )
        .expect("axes are canonical")
    }

    /// The entries every indexed test ranks: `(key, the sum items of its
    /// subtree)`. One subtree is empty and one sum is negative.
    fn entries() -> Vec<(&'static [u8], Vec<i64>)> {
        vec![
            (b"a", vec![10]),
            (b"b", vec![-1, -2]),
            (b"c", vec![3, 4, 5]),
            (b"d", vec![]),
            (b"e", vec![500]),
        ]
    }

    fn add_items(db: &GroveDb, key: &[u8], items: &[i64], v: &GroveVersion) {
        for (i, item) in items.iter().enumerate() {
            insert(
                db,
                &[TEST_LEAF, TREE, key],
                &[i as u8],
                Element::new_sum_item(*item),
                v,
            );
        }
    }

    /// A `ProvableSumIndexedTree` whose entries are sum trees, and a sum
    /// item entered directly.
    fn build_psit(db: &GroveDb, v: &GroveVersion) {
        insert(
            db,
            &[TEST_LEAF],
            TREE,
            Element::empty_provable_sum_indexed_tree(),
            v,
        );
        for (key, items) in entries() {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, TREE].as_ref(),
                key,
                Element::empty_sum_tree(),
                None,
                v,
            )
            .unwrap()
            .expect("insert an entry");
            add_items(db, key, &items, v);
        }
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, TREE].as_ref(),
            b"s",
            Element::new_sum_item(99),
            None,
            v,
        )
        .unwrap()
        .expect("insert a sum item");
    }

    /// A `ProvableCountProvableSumIndexedTree` on the count, sum and avg
    /// axes whose entries are count sum trees.
    fn build_pcpsit(db: &GroveDb, v: &GroveVersion) {
        insert(
            db,
            &[TEST_LEAF],
            TREE,
            narrow_axes(&[IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg]),
            v,
        );
        for (key, items) in entries() {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, TREE].as_ref(),
                key,
                Element::empty_count_sum_tree(),
                None,
                v,
            )
            .unwrap()
            .expect("insert an entry");
            add_items(db, key, &items, v);
        }
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

    /// The tree's entries from the top of `axis`, read and proved: the proof
    /// must verify against the root and name the entries the read returns.
    fn top(db: &GroveDb, axis: IndexAxis, v: &GroveVersion) -> Vec<(i128, Vec<u8>)> {
        let path_query =
            PathQuery::new_axis_top_k(vec![TEST_LEAF.to_vec(), TREE.to_vec()], axis, 10, 0, true);
        let PathQueryRun::AxisEntries { entries: read, .. } = db
            .run_path_query(
                &path_query,
                true,
                true,
                true,
                QueryResultType::QueryKeyElementPairResultType,
                None,
                v,
            )
            .unwrap()
            .expect("read")
        else {
            panic!("expected axis entries");
        };
        let proof = db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove");
        let VerifiedPathQuery::AxisEntries {
            root_hash, entries, ..
        } = GroveDb::verify_path_query(&proof, &path_query, v).expect("verify")
        else {
            panic!("expected proved axis entries");
        };
        assert_eq!(root_hash, root(db, v));
        assert_eq!(entries, read, "the proof names what the read returns");
        ranking(entries)
    }

    /// Reconciling an indexed tree whose rows are the ones its primary calls
    /// for changes nothing.
    fn assert_rows_are_canonical(db: &GroveDb, v: &GroveVersion) {
        let before = root(db, v);
        db.reconcile_indexed_tree_secondaries([TEST_LEAF, TREE].as_ref(), None, v)
            .unwrap()
            .expect("reconcile");
        assert_eq!(root(db, v), before);
        assert_verifies(db, v);
    }

    #[test]
    fn a_sum_indexed_tree_ranks_on_the_big_sum_axis_once_converted() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        build_psit(&db, v);
        let ranked_by_sum = top(&db, IndexAxis::Sum, v);
        assert_eq!(ranked_by_sum.len(), 6);

        convert(&db, &[TEST_LEAF], TREE, v).expect("convert");

        let element = stored(&db, &[TEST_LEAF], TREE, v);
        assert!(matches!(element, Element::ProvableBigSumIndexedTree(..)));
        assert_eq!(sum_of(&element), 10 - 3 + 12 + 500 + 99);
        assert_eq!(top(&db, IndexAxis::BigSum, v), ranked_by_sum);
        assert_rows_are_canonical(&db, v);

        // An entry can now be a tree with a 128-bit sum, ranked by a sum
        // past the i64 range.
        db.insert_into_provable_big_sum_indexed_tree(
            [TEST_LEAF, TREE].as_ref(),
            b"z",
            Element::empty_big_sum_tree(),
            None,
            v,
        )
        .unwrap()
        .expect("insert a wide entry");
        add_items(&db, b"z", &[i64::MAX, i64::MAX], v);
        let ranked = top(&db, IndexAxis::BigSum, v);
        assert_eq!(ranked[0], (2 * MAX, b"z".to_vec()));
        assert_eq!(ranked[1..], ranked_by_sum[..]);
        assert_rows_are_canonical(&db, v);
    }

    #[test]
    fn a_count_and_sum_indexed_tree_keeps_each_ranking_once_converted() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        build_pcpsit(&db, v);
        let ranked_by_count = top(&db, IndexAxis::Count, v);
        let ranked_by_sum = top(&db, IndexAxis::Sum, v);
        let ranked_by_avg = top(&db, IndexAxis::Avg, v);

        convert(&db, &[TEST_LEAF], TREE, v).expect("convert");

        let element = stored(&db, &[TEST_LEAF], TREE, v);
        let Element::ProvableCountProvableBigSumIndexedTree(_, count, sum, axes, _) = &element
        else {
            panic!("expected the wide indexed tree, got {element:?}");
        };
        assert_eq!((*count, *sum), (7, 10 - 3 + 12 + 500));
        assert_eq!(
            axes.iter().map(|(tag, _)| *tag).collect::<Vec<_>>(),
            vec![
                IndexAxis::Count.tag(),
                IndexAxis::Avg.tag(),
                IndexAxis::BigSum.tag()
            ]
        );
        assert_eq!(top(&db, IndexAxis::Count, v), ranked_by_count);
        assert_eq!(top(&db, IndexAxis::BigSum, v), ranked_by_sum);
        assert_eq!(top(&db, IndexAxis::Avg, v), ranked_by_avg);
        assert_rows_are_canonical(&db, v);

        // The rankings follow a write made after the conversion.
        add_items(&db, b"d", &[1_000], v);
        assert_eq!(top(&db, IndexAxis::BigSum, v)[0], (1_000, b"d".to_vec()));
        assert_rows_are_canonical(&db, v);
    }

    #[test]
    fn an_entry_of_an_indexed_tree_is_converted_under_a_wide_parent_only() {
        let v = GroveVersion::latest();

        // Under a 64-bit indexed tree the entry adds its sum to the parent.
        let db = make_test_grovedb(v);
        build_psit(&db, v);
        let before = root(&db, v);
        assert!(matches!(
            convert(&db, &[TEST_LEAF, TREE], b"a", v),
            Err(Error::InvalidInput(_))
        ));
        assert_eq!(root(&db, v), before);

        // Once the parent is converted the entry can follow, and its row
        // follows the element.
        convert(&db, &[TEST_LEAF], TREE, v).expect("convert the parent");
        let ranked = top(&db, IndexAxis::BigSum, v);
        convert(&db, &[TEST_LEAF, TREE], b"a", v).expect("convert the entry");
        assert_eq!(
            stored(&db, &[TEST_LEAF, TREE], b"a", v),
            Element::BigSumTree(Some(vec![0]), 10, None)
        );
        assert_eq!(top(&db, IndexAxis::BigSum, v), ranked);
        assert_rows_are_canonical(&db, v);

        add_items(&db, b"a", &[5, i64::MAX, i64::MAX], v);
        assert_eq!(
            top(&db, IndexAxis::BigSum, v)[0],
            (2 * MAX + 5, b"a".to_vec())
        );
        assert_rows_are_canonical(&db, v);
    }

    #[test]
    fn an_indexed_tree_that_is_an_entry_of_another_is_converted() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        let outer: &[&[u8]] = &[TEST_LEAF, TREE];
        let inner: &[&[u8]] = &[TEST_LEAF, TREE, b"inner"];

        // A 128-bit indexed tree ranking a sum item and a 64-bit indexed
        // tree, which ranks two sum items of its own.
        insert(
            &db,
            &[TEST_LEAF],
            TREE,
            Element::empty_provable_big_sum_indexed_tree(),
            v,
        );
        for (key, entry) in [
            (b"item".as_slice(), Element::new_sum_item(40)),
            (
                b"inner".as_slice(),
                Element::empty_provable_sum_indexed_tree(),
            ),
        ] {
            db.insert_into_provable_big_sum_indexed_tree(outer, key, entry, None, v)
                .unwrap()
                .expect("insert an outer entry");
        }
        for (key, sum) in [(b"x", 100), (b"y", -30)] {
            db.insert_into_provable_sum_indexed_tree(
                inner,
                key,
                Element::new_sum_item(sum),
                None,
                v,
            )
            .unwrap()
            .expect("insert an inner entry");
        }
        let ranked = top(&db, IndexAxis::BigSum, v);
        assert_eq!(
            ranked,
            vec![(70, b"inner".to_vec()), (40, b"item".to_vec())]
        );

        convert(&db, outer, b"inner", v).expect("convert the inner tree");

        assert!(matches!(
            stored(&db, outer, b"inner", v),
            Element::ProvableBigSumIndexedTree(_, _, 70, _)
        ));
        // The outer tree's row for the inner tree follows the new element:
        // the ranking still proves, and reconciling finds nothing to do.
        assert_eq!(top(&db, IndexAxis::BigSum, v), ranked);
        assert_rows_are_canonical(&db, v);

        // The inner tree ranks on its big sum axis and takes a wide entry.
        db.insert_into_provable_big_sum_indexed_tree(
            inner,
            b"z",
            Element::empty_big_sum_tree(),
            None,
            v,
        )
        .unwrap()
        .expect("insert a wide entry");
        for i in 0..2u8 {
            insert(
                &db,
                &[TEST_LEAF, TREE, b"inner", b"z"],
                &[i],
                Element::new_sum_item(i64::MAX),
                v,
            );
        }
        assert_eq!(
            top(&db, IndexAxis::BigSum, v)[0],
            (2 * MAX + 70, b"inner".to_vec())
        );
        assert_rows_are_canonical(&db, v);
        db.reconcile_indexed_tree_secondaries(inner, None, v)
            .unwrap()
            .expect("reconcile the inner tree");
        assert_verifies(&db, v);
    }

    // ---- refusals ----

    #[test]
    fn only_a_tree_with_a_64_bit_sum_and_a_counterpart_is_converted() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        for (key, element) in [
            (b"tree".as_slice(), Element::empty_tree()),
            (b"count".as_slice(), Element::empty_count_tree()),
            (b"big".as_slice(), Element::empty_big_sum_tree()),
            (
                b"provable_count".as_slice(),
                Element::empty_provable_count_tree(),
            ),
            (
                b"count_indexed".as_slice(),
                Element::empty_provable_count_indexed_tree(),
            ),
            (b"item".as_slice(), Element::new_item(vec![1])),
        ] {
            insert(&db, &[TEST_LEAF], key, element.clone(), v);
            let before = root(&db, v);
            let result = convert(&db, &[TEST_LEAF], key, v);
            assert!(
                matches!(result, Err(Error::InvalidInput(_))),
                "{key:?}: {result:?}"
            );
            assert_eq!(root(&db, v), before);
            assert_eq!(stored(&db, &[TEST_LEAF], key, v), element);
        }

        assert!(convert(&db, &[TEST_LEAF], b"absent", v).is_err());
    }

    #[test]
    fn a_tree_its_parent_sums_in_64_bits_waits_for_the_parent() {
        let v = GroveVersion::latest();
        for (name, narrow, _) in plain_trees() {
            let db = make_test_grovedb(v);
            insert(&db, &[TEST_LEAF], b"parent", narrow.clone(), v);
            let parent: &[&[u8]] = &[TEST_LEAF, b"parent"];
            // An empty child is refused as a filled one is.
            insert(&db, parent, b"empty", Element::empty_sum_tree(), v);
            insert(&db, parent, b"filled", Element::empty_sum_tree(), v);
            insert(
                &db,
                &[TEST_LEAF, b"parent", b"filled"],
                b"a",
                Element::new_sum_item(9),
                v,
            );
            insert(
                &db,
                parent,
                b"not_summed",
                Element::new_not_summed(Element::empty_sum_tree()).expect("wrap"),
                v,
            );

            let before = root(&db, v);
            for key in [b"empty".as_slice(), b"filled".as_slice()] {
                let result = convert(&db, parent, key, v);
                assert!(
                    matches!(result, Err(Error::InvalidInput(_))),
                    "{name} {key:?}: {result:?}"
                );
            }
            assert_eq!(root(&db, v), before, "{name}");

            // A child the parent leaves out of its sum adds nothing either
            // way.
            convert(&db, parent, b"not_summed", v).expect(name);
            assert_eq!(
                stored(&db, parent, b"not_summed", v),
                Element::new_not_summed(Element::empty_big_sum_tree()).expect("wrap"),
                "{name}"
            );

            // Parent first, then the children: the parent's sum is the same
            // before and after each step.
            convert(&db, &[TEST_LEAF], b"parent", v).expect(name);
            assert_eq!(
                sum_of(&stored(&db, &[TEST_LEAF], b"parent", v)),
                9,
                "{name}"
            );
            for key in [b"empty".as_slice(), b"filled".as_slice()] {
                convert(&db, parent, key, v).expect(name);
            }
            assert_eq!(
                sum_of(&stored(&db, &[TEST_LEAF], b"parent", v)),
                9,
                "{name}"
            );
            insert(
                &db,
                &[TEST_LEAF, b"parent", b"filled"],
                b"b",
                Element::new_sum_item(i64::MAX),
                v,
            );
            assert_eq!(
                sum_of(&stored(&db, &[TEST_LEAF], b"parent", v)),
                9 + MAX,
                "{name}"
            );
            assert_verifies(&db, v);
        }
    }

    #[test]
    fn a_tree_holding_a_child_it_would_sum_differently_is_refused() {
        let v = GroveVersion::latest();
        // A tree with a 128-bit sum adds nothing to a tree with a 64-bit
        // sum and its whole sum to one with a 128-bit sum.
        for (name, narrow, _) in plain_trees() {
            for (child_name, child) in [
                ("BigSumTree", Element::empty_big_sum_tree()),
                ("CountBigSumTree", Element::empty_count_big_sum_tree()),
            ] {
                let db = make_test_grovedb(v);
                insert(&db, &[TEST_LEAF], TREE, narrow.clone(), v);
                insert(&db, &[TEST_LEAF, TREE], b"a", Element::new_sum_item(4), v);
                insert(&db, &[TEST_LEAF, TREE], b"wide", child, v);
                for i in 0..3u8 {
                    insert(
                        &db,
                        &[TEST_LEAF, TREE, b"wide"],
                        &[i],
                        Element::new_sum_item(i64::MAX),
                        v,
                    );
                }
                let element = stored(&db, &[TEST_LEAF], TREE, v);
                assert_eq!(
                    element.sum_value_or_default(),
                    4,
                    "{name} holding a {child_name}"
                );
                let before = root(&db, v);

                let result = convert(&db, &[TEST_LEAF], TREE, v);
                assert!(
                    matches!(result, Err(Error::InvalidInput(_))),
                    "{name} holding a {child_name}: {result:?}"
                );
                assert_eq!(root(&db, v), before);
                assert_eq!(stored(&db, &[TEST_LEAF], TREE, v), element);
            }
        }
    }

    #[test]
    fn rows_of_both_signs_are_rebuilt_without_passing_through_a_sum_that_overflows() {
        let v = GroveVersion::latest();
        const BIG: i64 = 1 << 61;

        // Sum axis: without the most negative row the others total 2^63,
        // which no i64 holds.
        let db = make_test_grovedb(v);
        insert(
            &db,
            &[TEST_LEAF],
            TREE,
            Element::empty_provable_sum_indexed_tree(),
            v,
        );
        for (key, sum) in [
            (b"e", -2 * BIG),
            (b"a", BIG),
            (b"b", BIG),
            (b"c", BIG),
            (b"d", BIG),
        ] {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, TREE].as_ref(),
                key,
                Element::new_sum_item(sum),
                None,
                v,
            )
            .unwrap()
            .expect("insert an entry");
        }
        let ranked = top(&db, IndexAxis::Sum, v);
        convert(&db, &[TEST_LEAF], TREE, v).expect("convert the sum indexed tree");
        assert_eq!(top(&db, IndexAxis::BigSum, v), ranked);
        assert_eq!(
            sum_of(&stored(&db, &[TEST_LEAF], TREE, v)),
            2 * i128::from(BIG)
        );
        assert_rows_are_canonical(&db, v);

        // Avg axis: its rows carry the entry's sum until the conversion and
        // 0 after it, and with the -1 gone first the rest pass i64::MAX.
        let db = make_test_grovedb(v);
        insert(
            &db,
            &[TEST_LEAF],
            TREE,
            narrow_axes(&[IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg]),
            v,
        );
        for (key, sum) in [(b"n", -1), (b"p", 1), (b"x", i64::MAX)] {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, TREE].as_ref(),
                key,
                Element::new_sum_item(sum),
                None,
                v,
            )
            .unwrap()
            .expect("insert an entry");
        }
        let ranked_by_sum = top(&db, IndexAxis::Sum, v);
        let ranked_by_avg = top(&db, IndexAxis::Avg, v);
        convert(&db, &[TEST_LEAF], TREE, v).expect("convert the count and sum indexed tree");
        assert_eq!(top(&db, IndexAxis::BigSum, v), ranked_by_sum);
        assert_eq!(top(&db, IndexAxis::Avg, v), ranked_by_avg);
        assert_eq!(sum_of(&stored(&db, &[TEST_LEAF], TREE, v)), MAX);
        assert_rows_are_canonical(&db, v);
    }

    #[test]
    fn the_cost_of_an_indexed_tree_reports_its_rows_once_each() {
        let v = GroveVersion::latest();
        // `(what each entry's sum is a multiple of, the bytes the five rows
        // lose with the sum they carried)`: a row of the sum axis carries
        // its entry's sum and a row of the big sum axis carries 0, which
        // takes one byte, as the sums 0..=4 do. The sums 0, 100, 200, 300
        // and 400 take 1, 1, 3, 3 and 3.
        for (scale, bytes_of_the_sums_dropped) in [(1, 0), (100, 6)] {
            let db = make_test_grovedb(v);
            insert(
                &db,
                &[TEST_LEAF],
                TREE,
                Element::empty_provable_sum_indexed_tree(),
                v,
            );
            for i in 0..5u8 {
                db.insert_into_provable_sum_indexed_tree(
                    [TEST_LEAF, TREE].as_ref(),
                    &[i],
                    Element::new_sum_item(i64::from(i) * scale),
                    None,
                    v,
                )
                .unwrap()
                .expect("insert an entry");
            }

            let conversion = db.convert_to_big_sum_tree([TEST_LEAF].as_ref(), TREE, None, v);
            conversion.value.expect("convert");
            let storage = conversion.cost.storage_cost;
            let net = i64::from(storage.added_bytes)
                - i64::from(storage.removed_bytes.total_removed_bytes());

            // The primary grows as any tree does: 8 bytes in each entry's
            // node, 8 in the link to it, 7 in the tree's element. Each of
            // the five rows is removed and written once, 8 bytes longer in
            // its key, which a row pays for in its node and in the link to
            // it.
            assert_eq!(
                net,
                5 * 16 + 7 + 5 * 16 - bytes_of_the_sums_dropped,
                "sums that are multiples of {scale}"
            );
        }
    }

    #[test]
    fn a_tree_written_past_the_checks_of_a_released_version_is_refused() {
        use grovedb_version::version::v4::GROVE_V4;

        use crate::batch::QualifiedGroveDbOp;

        let latest = GroveVersion::latest();
        let insert_op = |path: &[&[u8]], key: &[u8], element: Element| {
            QualifiedGroveDbOp::insert_or_replace_op(
                path.iter().map(|segment| segment.to_vec()).collect(),
                key.to_vec(),
                element,
            )
        };
        let refused_unchanged = |db: &GroveDb, name: &str| {
            let before = root(db, latest);
            let element = stored(db, &[TEST_LEAF], TREE, latest);
            let result = convert(db, &[TEST_LEAF], TREE, latest);
            assert!(
                matches!(result, Err(Error::InvalidInput(_))),
                "{name}: {result:?}"
            );
            assert_eq!(root(db, latest), before, "{name}");
            assert_eq!(stored(db, &[TEST_LEAF], TREE, latest), element, "{name}");
        };

        // On GROVE_V4 a batch that creates a tree and fills it skips the
        // checks the tree's insert makes alone. One is that an indexed
        // tree's axes are canonical.
        let db = make_test_grovedb(&GROVE_V4);
        let axis_twice = Element::ProvableCountProvableSumIndexedTree(
            None,
            0,
            0,
            vec![
                (IndexAxis::Count.tag(), None),
                (IndexAxis::Count.tag(), None),
            ],
            None,
        );
        db.apply_batch(
            vec![
                insert_op(&[TEST_LEAF], TREE, axis_twice),
                insert_op(&[TEST_LEAF, TREE], b"k", Element::new_sum_item(5)),
            ],
            None,
            None,
            &GROVE_V4,
        )
        .unwrap()
        .expect("GROVE_V4 takes a tree with an axis twice");
        refused_unchanged(&db, "an axis twice");

        // Another is what an indexed tree accepts as an entry: a plain tree
        // carries no sum.
        let db = make_test_grovedb(&GROVE_V4);
        insert(
            &db,
            &[TEST_LEAF],
            TREE,
            Element::empty_provable_sum_indexed_tree(),
            &GROVE_V4,
        );
        db.apply_batch(
            vec![
                insert_op(&[TEST_LEAF, TREE], b"k", Element::empty_tree()),
                insert_op(&[TEST_LEAF, TREE, b"k"], b"x", Element::new_item(vec![1])),
            ],
            None,
            None,
            &GROVE_V4,
        )
        .unwrap()
        .expect("GROVE_V4 takes a plain tree as an entry");
        refused_unchanged(&db, "an entry that carries no sum");

        // A third is the wrapper a parent takes: a tree with its count in
        // its node hashes holds no child left out of the count.
        let db = make_test_grovedb(&GROVE_V4);
        insert(
            &db,
            &[TEST_LEAF],
            TREE,
            Element::empty_provable_count_provable_sum_tree(),
            &GROVE_V4,
        );
        db.apply_batch(
            vec![
                insert_op(
                    &[TEST_LEAF, TREE],
                    b"k",
                    Element::new_non_counted(Element::empty_count_sum_tree()).expect("wrap"),
                ),
                insert_op(&[TEST_LEAF, TREE, b"k"], b"x", Element::new_sum_item(5)),
            ],
            None,
            None,
            &GROVE_V4,
        )
        .unwrap()
        .expect("GROVE_V4 takes a child left out of a provable count");
        refused_unchanged(&db, "a child left out of a provable count");
    }

    #[test]
    fn a_tree_whose_element_disagrees_with_it_is_refused() {
        let v = GroveVersion::latest();
        // An insert takes a tree element with a sum and no children as it
        // is. The tree holds nothing, so its element says 9 and it sums 0:
        // converted from the tree it would come out with 0, and from the
        // element with 9.
        for (name, claiming) in [
            ("SumTree", Element::SumTree(None, 9, None)),
            ("CountSumTree", Element::CountSumTree(None, 2, 9, None)),
        ] {
            let db = make_test_grovedb(v);
            insert(&db, &[TEST_LEAF], TREE, claiming.clone(), v);
            let before = root(&db, v);

            let result = convert(&db, &[TEST_LEAF], TREE, v);
            assert!(
                matches!(result, Err(Error::InvalidInput(_))),
                "{name}: {result:?}"
            );
            assert_eq!(root(&db, v), before, "{name}");
            assert_eq!(stored(&db, &[TEST_LEAF], TREE, v), claiming, "{name}");

            // Once a write has brought the element in line with the tree,
            // it converts.
            insert(&db, &[TEST_LEAF, TREE], b"a", Element::new_sum_item(4), v);
            convert(&db, &[TEST_LEAF], TREE, v).expect(name);
            assert_eq!(sum_of(&stored(&db, &[TEST_LEAF], TREE, v)), 4, "{name}");
        }
    }

    #[test]
    fn an_indexed_tree_with_a_key_the_big_sum_axis_cannot_rank_is_refused() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        insert(
            &db,
            &[TEST_LEAF],
            TREE,
            Element::empty_provable_sum_indexed_tree(),
            v,
        );
        // The sum axis ranks keys of up to 247 bytes, the big sum axis keys
        // of up to 239.
        for key in [vec![1u8; 239], vec![2u8; 240]] {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, TREE].as_ref(),
                &key,
                Element::new_sum_item(1),
                None,
                v,
            )
            .unwrap()
            .expect("insert a long key");
        }
        let before = root(&db, v);

        let result = convert(&db, &[TEST_LEAF], TREE, v);
        assert!(
            matches!(result, Err(Error::NotSupported(_))),
            "got {result:?}"
        );
        assert_eq!(root(&db, v), before);
        assert!(matches!(
            stored(&db, &[TEST_LEAF], TREE, v),
            Element::ProvableSumIndexedTree(..)
        ));
        assert_verifies(&db, v);

        // Without that key the tree converts.
        db.delete_from_provable_sum_indexed_tree(
            [TEST_LEAF, TREE].as_ref(),
            &vec![2u8; 240],
            None,
            v,
        )
        .unwrap()
        .expect("remove the long key");
        convert(&db, &[TEST_LEAF], TREE, v).expect("convert");
        assert_eq!(top(&db, IndexAxis::BigSum, v), vec![(1, vec![1u8; 239])]);
    }

    #[test]
    fn the_conversion_is_gated_to_the_version_that_introduces_it() {
        use grovedb_version::version::{v1::GROVE_V1, v2::GROVE_V2, v3::GROVE_V3, v4::GROVE_V4};

        for grove_version in [&GROVE_V1, &GROVE_V2, &GROVE_V3, &GROVE_V4] {
            let db = make_test_grovedb(grove_version);
            insert(
                &db,
                &[TEST_LEAF],
                TREE,
                Element::empty_sum_tree(),
                grove_version,
            );
            let result = convert(&db, &[TEST_LEAF], TREE, grove_version);
            assert!(
                matches!(
                    result,
                    Err(Error::VersionError(
                        grovedb_version::error::GroveVersionError::UnknownVersionMismatch { .. }
                    ))
                ),
                "v{}: {result:?}",
                grove_version.protocol_version
            );
            assert_eq!(
                stored(&db, &[TEST_LEAF], TREE, grove_version),
                Element::empty_sum_tree()
            );
        }
    }
}
