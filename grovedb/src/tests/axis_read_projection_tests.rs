//! `AxisProjection::Keys` on the unified PathQuery: `run_path_query`
//! returns the ranking pairs straight from the pinned secondary view
//! (no primary value resolved), as a strict projection of what the
//! `Entries` read — and the proof — return.

#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::query::{AggregateFold, AxisProjection, AxisQuery, IndexAxis};
    use grovedb_version::version::GroveVersion;

    use crate::{
        operations::{
            get::PathQueryRun,
            proof::{indexed_axis::AxisEntries, VerifiedPathQuery},
        },
        query_result_type::{AxisKeys, QueryResultType},
        tests::{make_test_grovedb, TEST_LEAF},
        Element, GroveDb, PathQuery,
    };

    const PCPSIT: &[u8] = b"pcpsit";

    /// A three-axis (count, sum, avg) indexed tree with `(key, sum)`
    /// entries.
    fn build(db: &GroveDb, gv: &GroveVersion, entries: &[(&[u8], i64)]) {
        let axes: Vec<(u8, Option<Vec<u8>>)> = vec![
            (IndexAxis::Count.tag(), None),
            (IndexAxis::Sum.tag(), None),
            (IndexAxis::Avg.tag(), None),
        ];
        db.insert(
            [TEST_LEAF].as_ref(),
            PCPSIT,
            Element::empty_provable_count_provable_sum_indexed_tree(axes).expect("canonical axes"),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("create pcpsit");
        for (key, sum) in entries {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, PCPSIT].as_ref(),
                key,
                Element::new_item_with_sum_item(b"v".to_vec(), *sum),
                None,
                gv,
            )
            .unwrap()
            .expect("insert entry");
        }
    }

    /// Two branch trees `[TEST_LEAF, branch, "scores"]` (PSITs); a third
    /// branch key is left absent.
    fn build_branched(db: &GroveDb, gv: &GroveVersion, branches: &[(&[u8], &[(&[u8], i64)])]) {
        for (branch, entries) in branches {
            db.insert(
                [TEST_LEAF].as_ref(),
                branch,
                Element::empty_tree(),
                None,
                None,
                gv,
            )
            .unwrap()
            .expect("create branch tree");
            db.insert(
                [TEST_LEAF, branch].as_ref(),
                b"scores",
                Element::empty_provable_sum_indexed_tree(),
                None,
                None,
                gv,
            )
            .unwrap()
            .expect("create branch PSIT");
            for (k, s) in *entries {
                db.insert_into_provable_sum_indexed_tree(
                    [TEST_LEAF, branch, b"scores".as_slice()].as_ref(),
                    k,
                    Element::new_sum_item(*s),
                    None,
                    gv,
                )
                .unwrap()
                .expect("insert branch entry");
            }
        }
    }

    fn path() -> Vec<Vec<u8>> {
        vec![TEST_LEAF.to_vec(), PCPSIT.to_vec()]
    }

    fn run(db: &GroveDb, pq: &PathQuery, gv: &GroveVersion) -> PathQueryRun {
        db.run_path_query(
            pq,
            true,
            true,
            true,
            QueryResultType::QueryKeyElementPairResultType,
            None,
            gv,
        )
        .unwrap()
        .expect("run_path_query")
    }

    fn entries_of(run: PathQueryRun) -> AxisEntries {
        match run {
            PathQueryRun::AxisEntries(e) => e,
            other => panic!("expected AxisEntries, got {other:?}"),
        }
    }

    fn keys_of(run: PathQueryRun) -> AxisKeys {
        match run {
            PathQueryRun::AxisKeys(k) => k,
            other => panic!("expected AxisKeys, got {other:?}"),
        }
    }

    /// Single-path: the keys read equals the entries read projected, on
    /// every axis, both directions, with and without an offset, for
    /// both entry-listing traversals.
    #[test]
    fn keys_projection_equals_entries_projected_on_every_axis() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build(
            &db,
            gv,
            &[(b"a", 50), (b"b", 10), (b"c", 30), (b"d", 20), (b"e", 40)],
        );

        for axis in [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg] {
            for descending in [true, false] {
                for offset in [0u64, 2] {
                    let entries_q = AxisQuery::top_k(axis, 2, offset, descending);
                    let keys_q = entries_q.clone().keys_only();
                    let entries = entries_of(run(&db, &PathQuery::new_axis(path(), entries_q), gv));
                    let keys = keys_of(run(&db, &PathQuery::new_axis(path(), keys_q), gv));
                    assert_eq!(
                        keys,
                        entries.to_keys(),
                        "{axis:?} descending={descending} offset={offset}"
                    );
                    assert_eq!(keys.len(), entries.len());
                }
                let entries_q = AxisQuery::bounded(axis, i128::MIN, i128::MAX, 3, descending);
                let keys_q = entries_q.clone().keys_only();
                let entries = entries_of(run(&db, &PathQuery::new_axis(path(), entries_q), gv));
                let keys = keys_of(run(&db, &PathQuery::new_axis(path(), keys_q), gv));
                assert_eq!(
                    keys,
                    entries.to_keys(),
                    "{axis:?} bounded descending={descending}"
                );
            }
        }
    }

    /// Keys is a strict projection of what the proof authenticates: the
    /// prover and verifier treat a `Keys` query as `Entries`, and the
    /// unproved keys read equals the verified entries projected.
    #[test]
    fn keys_projection_agrees_with_the_verified_entries() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build(&db, gv, &[(b"a", 50), (b"b", 10), (b"c", 30)]);
        let keys_pq = PathQuery::new_axis(
            path(),
            AxisQuery::top_k(IndexAxis::Sum, 2, 0, true).keys_only(),
        );

        let keys = keys_of(run(&db, &keys_pq, gv));
        let proof = db.prove_query(&keys_pq, None, gv).unwrap().expect("prove");
        let VerifiedPathQuery::AxisEntries {
            root_hash, entries, ..
        } = GroveDb::verify_path_query(&proof, &keys_pq, gv).expect("verify")
        else {
            panic!("expected AxisEntries from verification");
        };
        assert_eq!(keys, entries.to_keys());
        assert_eq!(
            root_hash,
            db.root_hash(None, gv).unwrap().expect("root hash"),
            "a keys-projected query proves and verifies like an entries query"
        );
    }

    /// Branched: per branch, the keys read equals the entries read
    /// projected, and an absent branch is `None` in both.
    #[test]
    fn branched_keys_projection_mirrors_branched_entries_including_absence() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build_branched(
            &db,
            gv,
            &[
                (b"alice", &[(b"m1", 10), (b"m2", 30)]),
                (b"carol", &[(b"m1", 7)]),
            ],
        );
        let branch_keys = vec![b"alice".to_vec(), b"bob".to_vec(), b"carol".to_vec()];
        let entries_pq = PathQuery::new_branched_axis(
            vec![TEST_LEAF.to_vec()],
            branch_keys.clone(),
            vec![b"scores".to_vec()],
            AxisQuery::top_k(IndexAxis::Sum, 2, 0, true),
        );
        let keys_pq = PathQuery::new_branched_axis(
            vec![TEST_LEAF.to_vec()],
            branch_keys.clone(),
            vec![b"scores".to_vec()],
            AxisQuery::top_k(IndexAxis::Sum, 2, 0, true).keys_only(),
        );
        let PathQueryRun::BranchedAxisEntries(entry_branches) = run(&db, &entries_pq, gv) else {
            panic!("expected BranchedAxisEntries");
        };
        let PathQueryRun::BranchedAxisKeys(key_branches) = run(&db, &keys_pq, gv) else {
            panic!("expected BranchedAxisKeys");
        };
        assert_eq!(key_branches.len(), 3);
        for ((ek, e), (kk, k)) in entry_branches.iter().zip(key_branches.iter()) {
            assert_eq!(ek, kk);
            assert_eq!(
                k.as_ref(),
                e.as_ref().map(AxisEntries::to_keys).as_ref(),
                "branch {:?}",
                ek
            );
        }
        let bob = key_branches
            .iter()
            .find(|(k, _)| k == b"bob")
            .expect("bob listed");
        assert!(
            bob.1.is_none(),
            "an absent branch is None under the keys projection too"
        );
    }

    /// The keys projection never opens the primary: fewer seeks and
    /// loaded bytes than the entries read of the same page.
    #[test]
    fn keys_projection_does_not_read_primaries() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build(
            &db,
            gv,
            &[(b"a", 50), (b"b", 10), (b"c", 30), (b"d", 20), (b"e", 40)],
        );
        let entries_q = AxisQuery::top_k(IndexAxis::Sum, 3, 1, true);
        let keys_q = entries_q.clone().keys_only();
        let entries_cost = db
            .run_path_query(
                &PathQuery::new_axis(path(), entries_q),
                true,
                true,
                true,
                QueryResultType::QueryKeyElementPairResultType,
                None,
                gv,
            )
            .cost;
        let keys_cost = db
            .run_path_query(
                &PathQuery::new_axis(path(), keys_q),
                true,
                true,
                true,
                QueryResultType::QueryKeyElementPairResultType,
                None,
                gv,
            )
            .cost;
        assert!(
            keys_cost.seek_count < entries_cost.seek_count,
            "keys {} vs entries {}",
            keys_cost.seek_count,
            entries_cost.seek_count
        );
        assert!(keys_cost.storage_loaded_bytes < entries_cost.storage_loaded_bytes);
    }

    /// A keys projection on a traversal that lists no entries is rejected
    /// at the query boundary rather than silently ignored.
    #[test]
    fn keys_projection_is_rejected_for_non_listing_traversals() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build(&db, gv, &[(b"a", 50), (b"b", 10)]);
        for q in [
            AxisQuery::rank_of_key(IndexAxis::Sum, b"a".to_vec(), true).keys_only(),
            AxisQuery::aggregate_over_value_range(IndexAxis::Sum, 0, 100, AggregateFold::Total)
                .keys_only(),
        ] {
            assert_eq!(q.projection, AxisProjection::Keys);
            let result = db
                .run_path_query(
                    &PathQuery::new_axis(path(), q),
                    true,
                    true,
                    true,
                    QueryResultType::QueryKeyElementPairResultType,
                    None,
                    gv,
                )
                .unwrap();
            assert!(
                result.is_err(),
                "keys projection must be rejected on a non-listing traversal"
            );
        }
    }
}
