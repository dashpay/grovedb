//! The refusals around `ProvableBigSumIndexedTree` and
//! `ProvableCountProvableBigSumIndexedTree`.
//!
//! `Element` is public, so `db.insert` and a batch can be handed a
//! hand-built indexed tree whose root keys, aggregates or axes disagree
//! with what is stored. Those fields are committed into the parent's hash,
//! so every one of them is checked before the write, as for the 64-bit
//! trees. The tests here drive each refusal for the 128-bit trees, and
//! with it the rules these trees start out with: the big sum axis is
//! served only from the grove version that introduces it, and a child
//! that carries backward references is refused.

#[cfg(test)]
mod tests {
    use grovedb_element::indexed::IndexAxis;
    use grovedb_merk::proofs::query::{AggregateFold, AxisQuery};
    use grovedb_version::version::{v3::GROVE_V3, v4::GROVE_V4, GroveVersion};

    use crate::{
        batch::QualifiedGroveDbOp,
        bidirectional_references::BackwardsReferences,
        operations::{
            get::{PathQueryRun, QueryItemOrSumReturnType},
            insert::InsertOptions,
            proof::indexed_axis::AxisEntries,
        },
        query_result_type::{AxisKeys, QueryResultType},
        reference_path::ReferencePathType,
        tests::{make_test_grovedb, TempGroveDb, ANOTHER_TEST_LEAF, TEST_LEAF},
        Element, Error, GroveDb, PathQuery,
    };

    const MAX: i128 = i64::MAX as i128;

    /// The entries each populated tree holds: `(key, sum)`.
    const ENTRIES: [(&[u8], i64); 3] = [(b"a", 5), (b"b", 7), (b"c", 11)];
    const TOTAL: i128 = 23;

    /// A root key no Merk here can have: nothing is stored under it, so a
    /// Merk opened with it is empty and reports no root key.
    fn bogus_root_key() -> Option<Vec<u8>> {
        Some(b"\xff\xff\xff\xff\xff\xff\xff\xff".to_vec())
    }

    fn override_tree() -> InsertOptions {
        InsertOptions {
            validate_insertion_does_not_override: false,
            validate_insertion_does_not_override_tree: false,
            base_root_storage_is_free: true,
            backwards_references: BackwardsReferences::DontCheck,
        }
    }

    fn create(db: &TempGroveDb, key: &[u8], element: Element, v: &GroveVersion) {
        db.insert([TEST_LEAF].as_ref(), key, element, None, None, v)
            .unwrap()
            .expect("create the tree");
    }

    /// Write `element` over the tree at `[TEST_LEAF, key]`.
    fn overwrite(
        db: &TempGroveDb,
        key: &[u8],
        element: Element,
        v: &GroveVersion,
    ) -> Result<(), Error> {
        db.insert(
            [TEST_LEAF].as_ref(),
            key,
            element,
            Some(override_tree()),
            None,
            v,
        )
        .unwrap()
    }

    #[track_caller]
    fn assert_invalid_input(result: Result<(), Error>, expected: &str) {
        match result {
            Err(Error::InvalidInput(message)) => assert!(
                message.contains(expected),
                "expected a message containing {expected:?}, got {message:?}"
            ),
            other => panic!("expected Err(InvalidInput(.. {expected:?} ..)), got {other:?}"),
        }
    }

    #[track_caller]
    fn assert_invalid_batch(result: Result<(), Error>, expected: &str) {
        match result {
            Err(Error::InvalidBatchOperation(message)) => assert!(
                message.contains(expected),
                "expected a message containing {expected:?}, got {message:?}"
            ),
            other => {
                panic!("expected Err(InvalidBatchOperation(.. {expected:?} ..)), got {other:?}")
            }
        }
    }

    fn assert_verifies(db: &GroveDb, v: &GroveVersion) {
        let issues = db.verify_grovedb(None, true, true, v).expect("verify");
        assert!(issues.is_empty(), "issues: {issues:?}");
    }

    fn stored(db: &GroveDb, key: &[u8], v: &GroveVersion) -> Element {
        db.get_raw([TEST_LEAF].as_ref().into(), key, None, v)
            .unwrap()
            .expect("the tree is stored")
    }

    /// A populated `ProvableBigSumIndexedTree` at `[TEST_LEAF, key]`:
    /// its `(primary_root_key, secondary_root_key, sum)`.
    fn populate_pbsit(
        db: &TempGroveDb,
        key: &[u8],
        v: &GroveVersion,
    ) -> (Option<Vec<u8>>, Option<Vec<u8>>, i128) {
        create(db, key, Element::empty_provable_big_sum_indexed_tree(), v);
        for (entry, sum) in ENTRIES {
            db.insert_into_provable_big_sum_indexed_tree(
                [TEST_LEAF, key].as_ref(),
                entry,
                Element::new_sum_item(sum),
                None,
                v,
            )
            .unwrap()
            .expect("insert an entry");
        }
        match stored(db, key, v) {
            Element::ProvableBigSumIndexedTree(primary, secondary, sum, _) => {
                assert!(primary.is_some() && secondary.is_some());
                assert_eq!(sum, TOTAL);
                (primary, secondary, sum)
            }
            other => panic!("expected a ProvableBigSumIndexedTree, got {other:?}"),
        }
    }

    /// A populated `ProvableCountProvableBigSumIndexedTree` on the count
    /// and big sum axes: its `(primary_root_key, count, sum, axes)`.
    #[allow(clippy::type_complexity)]
    fn populate_pcpbsit(
        db: &TempGroveDb,
        key: &[u8],
        v: &GroveVersion,
    ) -> (Option<Vec<u8>>, u64, i128, Vec<(u8, Option<Vec<u8>>)>) {
        create(
            db,
            key,
            Element::empty_provable_count_provable_big_sum_indexed_tree(vec![
                (IndexAxis::Count.tag(), None),
                (IndexAxis::BigSum.tag(), None),
            ])
            .expect("canonical axes"),
            v,
        );
        for (entry, sum) in ENTRIES {
            db.insert_into_provable_count_provable_big_sum_indexed_tree(
                [TEST_LEAF, key].as_ref(),
                entry,
                Element::new_item_with_sum_item(entry.to_vec(), sum),
                None,
                v,
            )
            .unwrap()
            .expect("insert an entry");
        }
        match stored(db, key, v) {
            Element::ProvableCountProvableBigSumIndexedTree(primary, count, sum, axes, _) => {
                assert!(primary.is_some());
                assert_eq!((count, sum), (3, TOTAL));
                assert!(axes.iter().all(|(_, root)| root.is_some()));
                (primary, count, sum, axes)
            }
            other => panic!("expected a ProvableCountProvableBigSumIndexedTree, got {other:?}"),
        }
    }

    /// A populated plain tree at `[TEST_LEAF, key]`, built from `empty`.
    fn populate_plain(db: &TempGroveDb, key: &[u8], empty: Element, v: &GroveVersion) {
        create(db, key, empty, v);
        for (entry, sum) in ENTRIES {
            db.insert(
                [TEST_LEAF, key].as_ref(),
                entry,
                Element::new_item_with_sum_item(entry.to_vec(), sum),
                None,
                None,
                v,
            )
            .unwrap()
            .expect("insert an item");
        }
    }

    // ---------------------------------------------------------------
    // Direct insert of a non-empty claim.
    // ---------------------------------------------------------------

    #[test]
    fn a_big_sum_indexed_claim_is_checked_against_what_is_stored() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        let (primary, secondary, sum) = populate_pbsit(&db, b"wide", v);
        let before = db.root_hash(None, v).unwrap().expect("root");
        let tree = |primary, secondary, sum| {
            Element::ProvableBigSumIndexedTree(primary, secondary, sum, None)
        };

        // A claim that is not wholly empty names both root keys.
        for (p, s, claimed) in [
            (bogus_root_key(), None, 0i128),
            (None, bogus_root_key(), 0),
            (None, None, 9),
        ] {
            let result = db
                .insert(
                    [TEST_LEAF].as_ref(),
                    b"partial",
                    tree(p, s, claimed),
                    None,
                    None,
                    v,
                )
                .unwrap();
            assert_invalid_input(result, "partial state is not permitted");
        }
        assert!(!db
            .has_raw([TEST_LEAF].as_ref(), b"partial", None, v)
            .unwrap()
            .expect("has_raw"));

        assert_invalid_input(
            overwrite(
                &db,
                b"wide",
                tree(bogus_root_key(), secondary.clone(), sum),
                v,
            ),
            "primary_root_key does not match the existing primary Merk's root key",
        );
        assert_invalid_input(
            overwrite(
                &db,
                b"wide",
                tree(primary.clone(), bogus_root_key(), sum),
                v,
            ),
            "does not match the stored element's canonical secondary root key",
        );
        // A sum the primary does not hold, one past the `i64` range
        // included.
        for forged in [sum + 1, 3 * MAX] {
            assert_invalid_input(
                overwrite(
                    &db,
                    b"wide",
                    tree(primary.clone(), secondary.clone(), forged),
                    v,
                ),
                "sum_value does not match the existing primary Merk's aggregate sum",
            );
        }

        // The claim is refused over a tree of another kind: a 64-bit
        // indexed tree, whose primary is not a big sum one, and a plain
        // big sum tree, which has no rows for the index.
        create(
            &db,
            b"narrow",
            Element::empty_provable_sum_indexed_tree(),
            v,
        );
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"narrow"].as_ref(),
            b"a",
            Element::new_sum_item(5),
            None,
            v,
        )
        .unwrap()
        .expect("insert an entry");
        let Element::ProvableSumIndexedTree(narrow_primary, ..) = stored(&db, b"narrow", v) else {
            panic!("expected a ProvableSumIndexedTree");
        };
        assert_invalid_input(
            overwrite(&db, b"narrow", tree(narrow_primary, bogus_root_key(), 5), v),
            "the existing primary Merk is not a provable-big-sum tree",
        );

        populate_plain(&db, b"plain", Element::empty_provable_big_sum_tree(), v);
        let Element::ProvableBigSumTree(plain_root, plain_sum, _) = stored(&db, b"plain", v) else {
            panic!("expected a ProvableBigSumTree");
        };
        assert_invalid_input(
            overwrite(
                &db,
                b"plain",
                tree(plain_root, bogus_root_key(), plain_sum),
                v,
            ),
            "an existing tree of a different type is stored at this key",
        );
        assert!(matches!(
            stored(&db, b"plain", v),
            Element::ProvableBigSumTree(..)
        ));

        // None of the refusals wrote anything to the tree, and the claim
        // that matches what is stored is accepted and changes nothing.
        overwrite(&db, b"wide", tree(primary, secondary, sum), v).expect("the honest claim");
        let ranked = PathQuery::new_axis_top_k(
            vec![TEST_LEAF.to_vec(), b"wide".to_vec()],
            IndexAxis::BigSum,
            10,
            0,
            true,
        );
        let PathQueryRun::AxisEntries { entries, .. } = db
            .run_path_query(
                &ranked,
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
        let AxisEntries::BigSum(entries) = entries else {
            panic!("expected big sum entries");
        };
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.ordering_value)
                .collect::<Vec<_>>(),
            vec![11, 7, 5]
        );
        assert_ne!(db.root_hash(None, v).unwrap().expect("root"), before);
        assert_verifies(&db, v);
    }

    #[test]
    fn a_count_and_big_sum_indexed_claim_is_checked_against_what_is_stored() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        let (primary, count, sum, axes) = populate_pcpbsit(&db, b"wide", v);
        let tree = |primary, count, sum, axes| {
            Element::ProvableCountProvableBigSumIndexedTree(primary, count, sum, axes, None)
        };

        // The axes are canonical whether or not the claim is empty: sorted,
        // without duplicates, and without the 64-bit sum axis.
        for bad_axes in [
            vec![
                (IndexAxis::BigSum.tag(), None),
                (IndexAxis::Count.tag(), None),
            ],
            vec![
                (IndexAxis::Count.tag(), None),
                (IndexAxis::Count.tag(), None),
            ],
            vec![(IndexAxis::Sum.tag(), None)],
            vec![],
        ] {
            let result = db
                .insert(
                    [TEST_LEAF].as_ref(),
                    b"bad_axes",
                    tree(None, 0, 0, bad_axes),
                    None,
                    None,
                    v,
                )
                .unwrap();
            assert_invalid_input(result, "axes must be canonical");
        }

        // A claim that is not wholly empty names its primary.
        for (claimed_count, claimed_sum, claimed_axes) in [
            (5u64, 0i128, vec![(IndexAxis::Count.tag(), None)]),
            (0, 9, vec![(IndexAxis::Count.tag(), None)]),
            (0, 0, vec![(IndexAxis::Count.tag(), bogus_root_key())]),
        ] {
            let result = db
                .insert(
                    [TEST_LEAF].as_ref(),
                    b"partial",
                    tree(None, claimed_count, claimed_sum, claimed_axes),
                    None,
                    None,
                    v,
                )
                .unwrap();
            assert_invalid_input(result, "partial state is not permitted");
        }
        for key in [b"bad_axes".as_slice(), b"partial"] {
            assert!(!db
                .has_raw([TEST_LEAF].as_ref(), key, None, v)
                .unwrap()
                .expect("has_raw"));
        }

        assert_invalid_input(
            overwrite(
                &db,
                b"wide",
                tree(bogus_root_key(), count, sum, axes.clone()),
                v,
            ),
            "primary_root_key does not match the existing primary Merk's root key",
        );
        assert_invalid_input(
            overwrite(
                &db,
                b"wide",
                tree(primary.clone(), count + 1, sum, axes.clone()),
                v,
            ),
            "count_value does not match the existing primary Merk's aggregate count",
        );
        assert_invalid_input(
            overwrite(
                &db,
                b"wide",
                tree(primary.clone(), count, 3 * MAX, axes.clone()),
                v,
            ),
            "sum_value does not match the existing primary Merk's aggregate sum",
        );

        // An axis cannot be dropped or added: no row would be rebuilt.
        let mut widened = axes.clone();
        widened.insert(1, (IndexAxis::Avg.tag(), None));
        for changed in [vec![axes[0].clone()], widened] {
            assert_invalid_input(
                overwrite(&db, b"wide", tree(primary.clone(), count, sum, changed), v),
                "the axes schema does not match the stored element",
            );
        }
        let mut tampered = axes.clone();
        tampered[1].1 = bogus_root_key();
        assert_invalid_input(
            overwrite(&db, b"wide", tree(primary.clone(), count, sum, tampered), v),
            "does not match the stored element's canonical axis root key",
        );

        // The claim is refused over a tree of another kind: an indexed
        // tree whose primary keeps no count, and a plain count and big sum
        // tree, which has no rows for the axes.
        let (other_primary, _, other_sum) = populate_pbsit(&db, b"sum_only", v);
        assert_invalid_input(
            overwrite(
                &db,
                b"sum_only",
                tree(other_primary, 3, other_sum, axes.clone()),
                v,
            ),
            "the existing primary Merk is not a provable-count-provable-big-sum tree",
        );

        populate_plain(
            &db,
            b"plain",
            Element::empty_provable_count_provable_big_sum_tree(),
            v,
        );
        let Element::ProvableCountProvableBigSumTree(plain_root, plain_count, plain_sum, _) =
            stored(&db, b"plain", v)
        else {
            panic!("expected a ProvableCountProvableBigSumTree");
        };
        assert_invalid_input(
            overwrite(
                &db,
                b"plain",
                tree(
                    plain_root,
                    plain_count,
                    plain_sum,
                    vec![(IndexAxis::Count.tag(), bogus_root_key())],
                ),
                v,
            ),
            "an existing tree of a different type is stored at this key",
        );
        assert!(matches!(
            stored(&db, b"plain", v),
            Element::ProvableCountProvableBigSumTree(..)
        ));

        // The claim that matches what is stored is accepted.
        overwrite(&db, b"wide", tree(primary, count, sum, axes), v).expect("the honest claim");
        assert_verifies(&db, v);
    }

    // ---------------------------------------------------------------
    // Batch creation.
    // ---------------------------------------------------------------

    fn apply(db: &GroveDb, op: QualifiedGroveDbOp, v: &GroveVersion) -> Result<(), Error> {
        db.apply_batch(vec![op], None, None, v).unwrap()
    }

    #[test]
    fn a_batch_creates_the_trees_empty_and_only_empty() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        let leaf = || vec![TEST_LEAF.to_vec()];
        let count_axes = || vec![(IndexAxis::Count.tag(), None)];
        let empty: [(&[u8], Element, &str); 2] = [
            (
                b"sum",
                Element::empty_provable_big_sum_indexed_tree(),
                "ProvableBigSumIndexedTree",
            ),
            (
                b"count_sum",
                Element::empty_provable_count_provable_big_sum_indexed_tree(count_axes())
                    .expect("canonical axes"),
                "ProvableCountProvableBigSumIndexedTree",
            ),
        ];

        // A tree that claims contents is refused.
        for claimed in [
            Element::ProvableBigSumIndexedTree(bogus_root_key(), bogus_root_key(), 0, None),
            Element::ProvableBigSumIndexedTree(None, None, 3 * MAX, None),
            Element::ProvableCountProvableBigSumIndexedTree(
                bogus_root_key(),
                0,
                0,
                count_axes(),
                None,
            ),
            Element::ProvableCountProvableBigSumIndexedTree(None, 1, 0, count_axes(), None),
            Element::ProvableCountProvableBigSumIndexedTree(None, 0, 3 * MAX, count_axes(), None),
            Element::ProvableCountProvableBigSumIndexedTree(
                None,
                0,
                0,
                vec![(IndexAxis::Count.tag(), bogus_root_key())],
                None,
            ),
        ] {
            assert_invalid_batch(
                apply(
                    &db,
                    QualifiedGroveDbOp::insert_or_replace_op(leaf(), b"claimed".to_vec(), claimed),
                    v,
                ),
                "must be empty at the moment of batch insertion",
            );
        }
        // So are axes that are not canonical, the 64-bit sum axis among
        // them.
        for bad_axes in [
            vec![
                (IndexAxis::BigSum.tag(), None),
                (IndexAxis::Count.tag(), None),
            ],
            vec![(IndexAxis::Sum.tag(), None)],
        ] {
            assert_invalid_batch(
                apply(
                    &db,
                    QualifiedGroveDbOp::insert_or_replace_op(
                        leaf(),
                        b"claimed".to_vec(),
                        Element::ProvableCountProvableBigSumIndexedTree(None, 0, 0, bad_axes, None),
                    ),
                    v,
                ),
                "must have canonical axes",
            );
        }
        assert!(!db
            .has_raw([TEST_LEAF].as_ref(), b"claimed", None, v)
            .unwrap()
            .expect("has_raw"));

        for (key, element, name) in empty {
            // A fresh key takes the tree.
            apply(
                &db,
                QualifiedGroveDbOp::insert_if_not_exists_op(leaf(), key.to_vec(), element.clone()),
                v,
            )
            .expect("create on a fresh key");
            assert_eq!(stored(&db, key, v), element);
            let created = db.root_hash(None, v).unwrap().expect("root");

            // A key that holds one refuses a second, or skips it.
            assert_invalid_batch(
                apply(
                    &db,
                    QualifiedGroveDbOp::insert_if_not_exists_op(
                        leaf(),
                        key.to_vec(),
                        element.clone(),
                    ),
                    v,
                ),
                &format!("attempting to insert {name} element that already exists"),
            );
            apply(
                &db,
                QualifiedGroveDbOp::insert_if_not_exists_or_skip_op(leaf(), key.to_vec(), element),
                v,
            )
            .expect("skip the existing key");
            assert_eq!(db.root_hash(None, v).unwrap().expect("root"), created);
        }
        assert_verifies(&db, v);
    }

    // ---------------------------------------------------------------
    // The rules the trees start out with.
    // ---------------------------------------------------------------

    /// Before the grove version that introduces the trees, a query that
    /// names the big sum axis is refused on every door, and before any
    /// storage is read: a path that holds nothing does not answer it.
    #[test]
    fn the_big_sum_axis_is_served_only_from_the_version_that_introduces_it() {
        let absent = vec![TEST_LEAF.to_vec(), b"absent".to_vec()];
        let queries = |axis: IndexAxis| {
            vec![
                PathQuery::new_axis_top_k(absent.clone(), axis, 10, 0, true),
                PathQuery::new_axis_bounded(absent.clone(), axis, 0, 10, 10, true),
                PathQuery::new_axis_rank_of_key(absent.clone(), axis, b"k".to_vec(), true),
                PathQuery::new_axis_aggregate_over_value_range(
                    absent.clone(),
                    axis,
                    0,
                    10,
                    AggregateFold::Population,
                ),
                PathQuery::new_axis(
                    absent.clone(),
                    AxisQuery::top_k(axis, 10, 0, true).keys_only(),
                ),
            ]
        };
        let read = |db: &GroveDb, query: &PathQuery, v: &GroveVersion| {
            db.run_path_query(
                query,
                true,
                true,
                true,
                QueryResultType::QueryKeyElementPairResultType,
                None,
                v,
            )
            .unwrap()
        };
        #[track_caller]
        fn assert_not_served<T: std::fmt::Debug>(result: Result<T, Error>) {
            match result {
                Err(Error::NotSupported(message)) => assert_eq!(
                    message,
                    "the big sum axis is not served at this grove version"
                ),
                other => panic!("expected the big sum axis to be refused, got {other:?}"),
            }
        }

        let v4 = &GROVE_V4;
        let db = make_test_grovedb(v4);
        for query in queries(IndexAxis::BigSum) {
            assert_not_served(read(&db, &query, v4));
            assert_not_served(db.prove_query(&query, None, v4).unwrap());
            assert_not_served(GroveDb::verify_path_query(&[], &query, v4));
        }

        // The other axes are served there as before, and the big sum axis
        // is served as they are from the version that introduces it.
        let v5 = GroveVersion::latest();
        let latest = make_test_grovedb(v5);
        for (old, new) in queries(IndexAxis::Count)
            .iter()
            .zip(queries(IndexAxis::BigSum).iter())
        {
            let served = format!("{:?}", read(&db, old, v4).map(|_| ()));
            assert!(!served.contains("big sum axis"), "{served}");
            assert_eq!(
                format!("{:?}", read(&latest, new, v5).map(|_| ())),
                format!("{:?}", read(&latest, old, v5).map(|_| ())),
            );
        }
    }

    /// A child that carries backward references has no proof node that
    /// also carries the aggregate, so a proof of the tree that holds it
    /// could not be verified. The plain provable trees refuse such a child;
    /// so do these.
    #[test]
    fn a_child_with_backward_references_is_refused() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        populate_pbsit(&db, b"sum", v);
        populate_pcpbsit(&db, b"count_sum", v);
        let before = db.root_hash(None, v).unwrap().expect("root");

        let sum_item = Element::new_sum_item_allowing_bidirectional_references(1);
        let item_with_sum = Element::ItemWithSumItemWithBackwardsReferences(
            b"x".to_vec(),
            1,
            Default::default(),
            None,
        );
        #[track_caller]
        fn assert_refused(result: Result<(), Error>) {
            let message = format!("{result:?}");
            assert!(
                result.is_err() && message.contains("may not live in Provable* aggregate trees"),
                "{message}"
            );
        }
        for child in [&sum_item, &item_with_sum] {
            assert_refused(
                db.insert_into_provable_big_sum_indexed_tree(
                    [TEST_LEAF, b"sum"].as_ref(),
                    b"k",
                    child.clone(),
                    None,
                    v,
                )
                .unwrap(),
            );
        }
        assert_refused(
            db.insert_into_provable_count_provable_big_sum_indexed_tree(
                [TEST_LEAF, b"count_sum"].as_ref(),
                b"k",
                item_with_sum,
                None,
                v,
            )
            .unwrap(),
        );
        // The count and sum tree takes a sum item only bare, so it refuses
        // this one before the rule above is reached.
        db.insert_into_provable_count_provable_big_sum_indexed_tree(
            [TEST_LEAF, b"count_sum"].as_ref(),
            b"k",
            sum_item,
            None,
            v,
        )
        .unwrap()
        .expect_err("a sum item with backward references");
        assert_eq!(db.root_hash(None, v).unwrap().expect("root"), before);

        // What the trees do hold is proved.
        for key in [b"sum".as_slice(), b"count_sum"] {
            let mut query = grovedb_merk::proofs::Query::new();
            query.insert_all();
            let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), key.to_vec()], query);
            let proof = db
                .prove_query(&path_query, None, v)
                .unwrap()
                .expect("prove");
            let (root_hash, results) =
                GroveDb::verify_query(&proof, &path_query, v).expect("verify");
            assert_eq!(root_hash, before);
            assert_eq!(results.len(), ENTRIES.len());
        }
    }

    // ---------------------------------------------------------------
    // Reads.
    // ---------------------------------------------------------------

    /// The single-path proofs the tests keep as a second opinion on the
    /// path query proofs cover the big sum axis: a rank and a population
    /// verify, and a total is refused as it is everywhere else.
    #[test]
    fn single_path_proofs_cover_the_big_sum_axis() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        populate_pbsit(&db, b"wide", v);
        let root = db.root_hash(None, v).unwrap().expect("root");
        let path: &[&[u8]] = &[TEST_LEAF, b"wide"];

        // Highest first: c (11), b (7), a (5).
        for (expected_rank, key) in [b"c".as_slice(), b"b", b"a"].into_iter().enumerate() {
            let (proof, rank) = db
                .prove_indexed_axis_rank_of_key(path, IndexAxis::BigSum, key, true, None, v)
                .unwrap()
                .expect("prove a rank");
            assert_eq!(rank, expected_rank as u64);
            let verified = GroveDb::verify_indexed_axis_rank_of_key(
                &proof,
                path,
                IndexAxis::BigSum,
                key,
                rank,
                true,
                v,
            )
            .expect("verify a rank");
            assert_eq!(verified.root_hash, root);
            // The proof does not verify as the rank of another key.
            GroveDb::verify_indexed_axis_rank_of_key(
                &proof,
                path,
                IndexAxis::BigSum,
                b"zz",
                rank,
                true,
                v,
            )
            .expect_err("another key");
        }

        let proof = db
            .prove_indexed_axis_aggregate_over_value_range(
                path,
                IndexAxis::BigSum,
                6,
                3 * MAX,
                AggregateFold::Population,
                None,
                v,
            )
            .unwrap()
            .expect("prove a population");
        let verified = GroveDb::verify_indexed_axis_aggregate_over_value_range(
            &proof,
            path,
            IndexAxis::BigSum,
            6,
            3 * MAX,
            AggregateFold::Population,
            v,
        )
        .expect("verify a population");
        assert_eq!(verified.root_hash, root);
        assert_eq!(verified.aggregate, 2, "b (7) and c (11)");
        // The proof answers the band it was made for and no other.
        GroveDb::verify_indexed_axis_aggregate_over_value_range(
            &proof,
            path,
            IndexAxis::BigSum,
            5,
            3 * MAX,
            AggregateFold::Population,
            v,
        )
        .expect_err("another band");

        db.prove_indexed_axis_aggregate_over_value_range(
            path,
            IndexAxis::BigSum,
            6,
            3 * MAX,
            AggregateFold::Total,
            None,
            v,
        )
        .unwrap()
        .expect_err("the big sum axis has no total");
        GroveDb::verify_indexed_axis_aggregate_over_value_range(
            &proof,
            path,
            IndexAxis::BigSum,
            6,
            3 * MAX,
            AggregateFold::Total,
            v,
        )
        .expect_err("a population is not a total");
    }

    /// A band of the big sum axis read as keys, and the tree after an entry
    /// is removed from it.
    #[test]
    fn a_band_of_the_big_sum_axis_is_read_as_keys() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        populate_pbsit(&db, b"wide", v);
        let path = vec![TEST_LEAF.to_vec(), b"wide".to_vec()];
        let band = |descending| {
            PathQuery::new_axis(
                path.clone(),
                AxisQuery::bounded(IndexAxis::BigSum, 6, 3 * MAX, 10, descending).keys_only(),
            )
        };
        let keys = |db: &GroveDb, query: &PathQuery| {
            let PathQueryRun::AxisKeys { keys, .. } = db
                .run_path_query(
                    query,
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
                panic!("expected axis keys");
            };
            keys
        };

        assert_eq!(
            keys(&db, &band(false)),
            AxisKeys::BigSum(vec![(7, b"b".to_vec()), (11, b"c".to_vec())])
        );
        assert_eq!(
            keys(&db, &band(true)),
            AxisKeys::BigSum(vec![(11, b"c".to_vec()), (7, b"b".to_vec())])
        );

        db.delete_from_provable_big_sum_indexed_tree([TEST_LEAF, b"wide"].as_ref(), b"c", None, v)
            .unwrap()
            .expect("remove an entry");
        assert_eq!(
            keys(&db, &band(true)),
            AxisKeys::BigSum(vec![(7, b"b".to_vec())])
        );
        assert_verifies(&db, v);
    }

    /// A read that returns each element's value or aggregate returns the
    /// count and the 128-bit sum of the trees that keep them, whether the
    /// tree is selected or a reference to it is.
    #[test]
    fn a_value_or_sum_read_returns_the_128_bit_aggregates() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        populate_plain(&db, b"a", Element::empty_count_big_sum_tree(), v);
        populate_plain(&db, b"b", Element::empty_provable_big_sum_tree(), v);
        populate_pbsit(&db, b"c", v);
        populate_pcpbsit(&db, b"d", v);
        let expected = vec![
            QueryItemOrSumReturnType::CountBigSumValue(3, TOTAL),
            QueryItemOrSumReturnType::BigSumValue(TOTAL),
            QueryItemOrSumReturnType::BigSumValue(TOTAL),
            QueryItemOrSumReturnType::CountBigSumValue(3, TOTAL),
        ];
        let all_under = |leaf: &[u8]| {
            let mut query = grovedb_merk::proofs::Query::new();
            query.insert_all();
            PathQuery::new_unsized(vec![leaf.to_vec()], query)
        };

        let (results, _) = db
            .query_item_value_or_sum(&all_under(TEST_LEAF), true, true, true, None, v)
            .unwrap()
            .expect("read the trees");
        assert_eq!(results, expected);

        // A reference to a tree can only be legacy data: it is written
        // under GROVE_V3, which still accepts one, and read under the
        // latest version.
        for key in [b"a", b"b", b"c", b"d"] {
            db.insert(
                [ANOTHER_TEST_LEAF].as_ref(),
                key,
                Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                    TEST_LEAF.to_vec(),
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
            .query_item_value_or_sum(&all_under(ANOTHER_TEST_LEAF), true, true, true, None, v)
            .unwrap()
            .expect("read through the references");
        assert_eq!(results, expected);
    }
}
