//! Coverage tests targeting uncovered diff lines of PR #657.
//!
//! Target file: `grovedb/src/operations/proof/generate.rs`.
//!
//! Clusters covered here:
//!
//! * The V0 prover's `NotSupported` arm for subqueries that cross an
//!   indexed tree (`ProvableCountIndexedTree` / `ProvableSumIndexedTree` /
//!   `ProvableCountProvableSumIndexedTree`). V0 is a frozen wire format,
//!   so it refuses cidx descent outright.
//! * `indexed_secondary_attestation`'s PCPSIT branch (the `axes_digest`
//!   over every configured axis's live secondary root hash) reached
//!   through the V1 terminal-attestation arm for a **populated** PCPSIT.
//!   The existing suite only covers the empty PCPSIT (which takes the
//!   empty-tree arm) and the populated PCIT / PSIT terminals.
//! * The aggregate-carrier rejections in the three V1 indexed-descent
//!   arms — an `AggregateCountOnRange` / `AggregateSumOnRange` /
//!   `AggregateCountAndSumOnRange` carrier query cannot descend through
//!   an indexed tree because the aggregate verifier only accepts
//!   `ProofBytes::Merk` and a two-input `combine_hash`.
//! * The `has_a_result_at_level` bookkeeping in the three V1
//!   indexed-descent arms, which only fires when a lower layer actually
//!   consumed the shared `overall_limit` (i.e. limit-bearing queries).
//! * The `overall_limit` decrement in the V1 indexed terminal arm.
//!
//! Every test either round-trips the proof (prove -> verify -> results
//! match `query_raw`) or asserts the exact designed rejection.

#[cfg(test)]
mod tests {
    use grovedb_element::indexed::IndexAxis;
    use grovedb_merk::proofs::Query;
    use grovedb_version::version::{v2::GROVE_V2, GroveVersion};

    use crate::{
        operations::proof::{GroveDBProof, ProofBytes},
        query::{PathQuery, SizedQuery},
        query_result_type::QueryResultType,
        tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
        Element, Error, GroveDb, QueryItem, SubqueryBranch,
    };

    // -----------------------------------------------------------------
    // Grove builders
    // -----------------------------------------------------------------

    /// Populated `ProvableCountIndexedTree` at `[TEST_LEAF, key]`.
    fn build_pcit(db: &GroveDb, key: &[u8], rows: &[&[u8]], v: &GroveVersion) {
        db.insert(
            [TEST_LEAF].as_ref(),
            key,
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("create PCIT");
        for row in rows {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, key].as_ref(),
                row,
                Element::new_item(b"v".to_vec()),
                None,
                v,
            )
            .unwrap()
            .expect("populate PCIT");
        }
    }

    /// Populated `ProvableSumIndexedTree` at `[TEST_LEAF, key]`.
    fn build_psit(db: &GroveDb, key: &[u8], rows: &[(&[u8], i64)], v: &GroveVersion) {
        db.insert(
            [TEST_LEAF].as_ref(),
            key,
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("create PSIT");
        for (row, sum) in rows {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, key].as_ref(),
                row,
                Element::new_sum_item(*sum),
                None,
                v,
            )
            .unwrap()
            .expect("populate PSIT");
        }
    }

    /// Populated `ProvableCountProvableSumIndexedTree` at
    /// `[TEST_LEAF, key]` over the supplied axis tags.
    fn build_pcpsit(
        db: &GroveDb,
        key: &[u8],
        axis_tags: &[u8],
        rows: &[(&[u8], i64)],
        v: &GroveVersion,
    ) {
        let axes: Vec<(u8, Option<Vec<u8>>)> = axis_tags.iter().map(|t| (*t, None)).collect();
        db.insert(
            [TEST_LEAF].as_ref(),
            key,
            Element::empty_provable_count_provable_sum_indexed_tree(axes)
                .expect("axes are canonical"),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("create PCPSIT");
        for (row, sum) in rows {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, key].as_ref(),
                row,
                Element::new_item_with_sum_item(b"v".to_vec(), *sum),
                None,
                v,
            )
            .unwrap()
            .expect("populate PCPSIT");
        }
    }

    fn all_axis_tags() -> Vec<u8> {
        vec![
            IndexAxis::Count.tag(),
            IndexAxis::Sum.tag(),
            IndexAxis::Avg.tag(),
        ]
    }

    // -----------------------------------------------------------------
    // Query builders
    // -----------------------------------------------------------------

    fn path_query_at_test_leaf(
        items: Vec<QueryItem>,
        subquery: Option<Query>,
        limit: Option<u16>,
    ) -> PathQuery {
        PathQuery {
            path: vec![TEST_LEAF.to_vec()],
            query: SizedQuery {
                query: Query {
                    items,
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: None,
                        subquery: subquery.map(|q| q.into()),
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                    add_parent_tree_on_subquery: false,
                },
                limit,
                offset: None,
            },
        }
    }

    /// `Key(key)` with no subquery — the indexed tree is itself the result.
    fn key_query(key: &[u8], limit: Option<u16>) -> PathQuery {
        path_query_at_test_leaf(vec![QueryItem::Key(key.to_vec())], None, limit)
    }

    /// `Key(key)` with an `insert_all` subquery — descends into the
    /// indexed tree's primary Merk.
    fn key_subquery(key: &[u8], limit: Option<u16>) -> PathQuery {
        let mut inner = Query::new();
        inner.insert_all();
        path_query_at_test_leaf(vec![QueryItem::Key(key.to_vec())], Some(inner), limit)
    }

    /// `RangeFull` with no subquery — every child of `TEST_LEAF` is a
    /// terminal result.
    fn range_full_query(limit: Option<u16>) -> PathQuery {
        path_query_at_test_leaf(vec![QueryItem::RangeFull(..)], None, limit)
    }

    /// Aggregate **carrier** shape: outer `Key(key)` routing to a leaf
    /// aggregate subquery.
    fn aggregate_carrier_query(key: &[u8], leaf: Query) -> PathQuery {
        path_query_at_test_leaf(vec![QueryItem::Key(key.to_vec())], Some(leaf), None)
    }

    // -----------------------------------------------------------------
    // Round-trip assertion
    // -----------------------------------------------------------------

    /// Prove, verify, and assert that the proved `(key, element)` results
    /// match what `query_raw` returns for the very same `PathQuery`.
    ///
    /// The reported *path* is compared only for non-terminal results: the
    /// V1 verifier reports an indexed terminal result under the path that
    /// already includes the element's own key, whereas `query_raw`
    /// reports it under the parent path. That difference is pre-existing
    /// verifier behavior and is not what these tests are locking.
    fn assert_proof_round_trips(db: &TempGroveDb, pq: &PathQuery, v: &GroveVersion) -> usize {
        let proof = db.prove_query(pq, None, v).unwrap().expect("prove_query");
        let (root_hash, results) = GroveDb::verify_query(&proof, pq, v).expect("verify_query");
        assert_eq!(
            root_hash,
            db.root_hash(None, v).unwrap().expect("root hash"),
            "verified root hash must match the live root"
        );

        let expected = db
            .query_raw(
                pq,
                true,
                true,
                true,
                QueryResultType::QueryPathKeyElementTrioResultType,
                None,
                v,
            )
            .unwrap()
            .expect("query_raw")
            .0
            .to_path_key_elements();

        let proved: Vec<(Vec<u8>, Element)> = results
            .iter()
            .map(|(_, key, element)| {
                (
                    key.clone(),
                    element
                        .clone()
                        .expect("proved result must carry an element"),
                )
            })
            .collect();
        let raw: Vec<(Vec<u8>, Element)> = expected
            .iter()
            .map(|(_, key, element)| (key.clone(), element.clone()))
            .collect();

        assert_eq!(
            proved, raw,
            "proved results must match query_raw results for {pq}"
        );

        // Paths: identical for every result that isn't an indexed tree
        // returned as a terminal (see the doc comment above).
        for ((proved_path, key, element), (raw_path, _, _)) in results.iter().zip(expected.iter()) {
            let is_indexed_terminal = matches!(
                element,
                Some(Element::ProvableCountIndexedTree(..))
                    | Some(Element::ProvableSumIndexedTree(..))
                    | Some(Element::ProvableCountProvableSumIndexedTree(..))
            );
            if !is_indexed_terminal {
                assert_eq!(
                    proved_path,
                    raw_path,
                    "proved path for key {} must match query_raw",
                    hex::encode(key)
                );
            }
        }
        proved.len()
    }

    fn decode_v1(proof_bytes: &[u8]) -> GroveDBProof {
        let (decoded, _): (GroveDBProof, _) =
            bincode::decode_from_slice(proof_bytes, bincode::config::standard())
                .expect("decode GroveDBProof");
        assert!(
            matches!(decoded, GroveDBProof::V1(_)),
            "latest grove version must produce a V1 envelope"
        );
        decoded
    }

    // =================================================================
    // 1. V0 prover: subqueries into indexed trees are NotSupported
    //    (generate.rs lines 749-762, guard at 752-753)
    // =================================================================
    //
    // The grove is built under the latest version (indexed trees are a
    // v3+ element family) and then *proved* under `GROVE_V2`, whose
    // `prove_query_non_serialized` dispatch value is 0. That is the only
    // way to reach the frozen V0 prover with an indexed element in the
    // walked layer.

    fn assert_v0_rejects_indexed_subquery(db: &TempGroveDb, key: &[u8]) {
        let v0: &GroveVersion = &GROVE_V2;
        let pq = key_subquery(key, None);
        let err = db
            .prove_query(&pq, None, v0)
            .unwrap()
            .expect_err("V0 must refuse to descend into an indexed tree");
        assert!(
            matches!(err, Error::NotSupported(ref msg)
                if msg.contains("V0 proofs do not support subqueries into")
                    && msg.contains("prove_query_v1")),
            "expected the V0 indexed-subquery NotSupported error, got {err:?}"
        );
    }

    #[test]
    fn v0_prover_rejects_subquery_into_populated_pcit() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        build_pcit(&db, b"pcit", &[b"a", b"b", b"c"], v);

        // The same query on V1 works — the rejection is V0-specific.
        let pq = key_subquery(b"pcit", None);
        assert_eq!(assert_proof_round_trips(&db, &pq, v), 3);

        assert_v0_rejects_indexed_subquery(&db, b"pcit");
    }

    #[test]
    fn v0_prover_rejects_subquery_into_populated_psit() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        build_psit(&db, b"psit", &[(b"a", 5), (b"b", -2)], v);

        let pq = key_subquery(b"psit", None);
        assert_eq!(assert_proof_round_trips(&db, &pq, v), 2);

        assert_v0_rejects_indexed_subquery(&db, b"psit");
    }

    #[test]
    fn v0_prover_rejects_subquery_into_populated_pcpsit() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        build_pcpsit(&db, b"pcpsit", &all_axis_tags(), &[(b"a", 4), (b"b", 6)], v);

        let pq = key_subquery(b"pcpsit", None);
        assert_eq!(assert_proof_round_trips(&db, &pq, v), 2);

        assert_v0_rejects_indexed_subquery(&db, b"pcpsit");
    }

    #[test]
    fn v0_prover_rejects_subquery_into_empty_indexed_tree() {
        // The V0 arm keys on the element variant, not on whether the
        // primary is populated: an empty indexed tree with a subquery is
        // refused just the same.
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        build_pcit(&db, b"pcit", &[], v);
        assert_v0_rejects_indexed_subquery(&db, b"pcit");
    }

    // =================================================================
    // 2. V1 terminal attestation for a POPULATED PCPSIT
    //    (generate.rs 2424-2431 incl. the PCPSIT pattern at 2427, and
    //     indexed_secondary_attestation's PCPSIT branch: 1383, 1405-1443)
    // =================================================================

    #[test]
    fn populated_pcpsit_terminal_attestation_round_trips_all_axis_subsets() {
        // A populated PCPSIT selected as a terminal result (no subquery
        // below it) takes the indexed-terminal arm, which asks
        // `indexed_secondary_attestation` for the axes_digest over every
        // configured axis's live secondary root hash. Exercise 1-, 2- and
        // 3-axis TLVs so the digest loop runs with several lengths.
        let v = GroveVersion::latest();
        let subsets: Vec<Vec<u8>> = vec![
            vec![IndexAxis::Count.tag()],
            vec![IndexAxis::Sum.tag()],
            vec![IndexAxis::Avg.tag()],
            vec![IndexAxis::Count.tag(), IndexAxis::Sum.tag()],
            all_axis_tags(),
        ];

        for axes in subsets {
            let db = make_test_grovedb(v);
            build_pcpsit(
                &db,
                b"pcpsit",
                &axes,
                &[(b"a", 10), (b"b", -3), (b"c", 7)],
                v,
            );

            let pq = key_query(b"pcpsit", None);
            let count = assert_proof_round_trips(&db, &pq, v);
            assert_eq!(
                count, 1,
                "the populated PCPSIT element itself is the single result (axes {axes:?})"
            );

            // The lower layer must be the 64-byte terminal envelope
            // (axes_digest ‖ primary_root), not a descent envelope.
            let proof_bytes = db.prove_query(&pq, None, v).unwrap().expect("prove");
            let GroveDBProof::V1(v1) = decode_v1(&proof_bytes) else {
                unreachable!("checked in decode_v1");
            };
            let terminal = v1
                .root_layer
                .lower_layers
                .values()
                .find_map(|layer| {
                    layer
                        .lower_layers
                        .values()
                        .find(|l| matches!(l.merk_proof, ProofBytes::IndexedTreeTerminal(_)))
                })
                .expect("populated PCPSIT must carry a terminal attestation");
            let ProofBytes::IndexedTreeTerminal(bytes) = &terminal.merk_proof else {
                unreachable!("matched above");
            };
            assert_eq!(
                bytes.len(),
                64,
                "terminal is axes_digest (32) ‖ primary_root (32), axes {axes:?}"
            );
            // A populated PCPSIT has at least one non-empty secondary, so
            // the axes_digest is a real digest over live roots rather than
            // the all-NULL_HASH digest of the empty form.
            assert_ne!(
                &bytes[..32],
                &[0u8; 32],
                "axes_digest over live secondaries must not be the zero hash (axes {axes:?})"
            );
        }
    }

    #[test]
    fn populated_pcpsit_terminal_attestation_is_bound_to_the_axes_digest() {
        // Flipping a byte of the attestation prefix must break the
        // three-input combine — this is what makes the axes_digest
        // computed in `indexed_secondary_attestation` load-bearing.
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        build_pcpsit(&db, b"pcpsit", &all_axis_tags(), &[(b"a", 1), (b"b", 2)], v);

        let pq = key_query(b"pcpsit", None);
        let proof_bytes = db.prove_query(&pq, None, v).unwrap().expect("prove");
        GroveDb::verify_query(&proof_bytes, &pq, v).expect("honest proof verifies");

        let GroveDBProof::V1(mut v1) = decode_v1(&proof_bytes) else {
            unreachable!("checked in decode_v1");
        };
        let terminal = v1
            .root_layer
            .lower_layers
            .values_mut()
            .find_map(|layer| {
                layer
                    .lower_layers
                    .values_mut()
                    .find(|l| matches!(l.merk_proof, ProofBytes::IndexedTreeTerminal(_)))
            })
            .expect("terminal attestation");
        if let ProofBytes::IndexedTreeTerminal(bytes) = &mut terminal.merk_proof {
            bytes[0] ^= 0xff;
        }
        let tampered = bincode::encode_to_vec(GroveDBProof::V1(v1), bincode::config::standard())
            .expect("encode");
        let result = GroveDb::verify_query(&tampered, &pq, v);
        assert!(
            matches!(result, Err(Error::InvalidProof(_, ref m))
                if m.contains("indexed terminal attestation")),
            "tampered axes_digest must be rejected, got {result:?}"
        );
    }

    #[test]
    fn populated_indexed_terminal_decrements_overall_limit() {
        // The terminal arm accounts for the element it emits against the
        // shared `overall_limit`, mirroring the verifier's decrement. With
        // two populated PCPSITs under TEST_LEAF and limit = 1, exactly one
        // may come back — matching `query_raw` under the same limit.
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        build_pcpsit(&db, b"pcpsit_a", &all_axis_tags(), &[(b"x", 3)], v);
        build_pcpsit(&db, b"pcpsit_b", &all_axis_tags(), &[(b"y", 9)], v);

        let unlimited = range_full_query(None);
        assert_eq!(
            assert_proof_round_trips(&db, &unlimited, v),
            2,
            "both PCPSITs are terminal results without a limit"
        );

        let limited = range_full_query(Some(1));
        assert_eq!(
            assert_proof_round_trips(&db, &limited, v),
            1,
            "the terminal arm must consume the single available limit slot"
        );
    }

    #[test]
    fn populated_pcit_and_psit_terminals_still_round_trip_with_a_limit() {
        // Same terminal arm, single-axis variants — `indexed_secondary
        // _attestation` takes its early-return path for these.
        let v = GroveVersion::latest();

        let db = make_test_grovedb(v);
        build_pcit(&db, b"pcit", &[b"a", b"b"], v);
        assert_eq!(
            assert_proof_round_trips(&db, &key_query(b"pcit", Some(1)), v),
            1
        );

        let db2 = make_test_grovedb(v);
        build_psit(&db2, b"psit", &[(b"a", 1), (b"b", 2)], v);
        assert_eq!(
            assert_proof_round_trips(&db2, &key_query(b"psit", Some(1)), v),
            1
        );
    }

    // =================================================================
    // 3. Aggregate-carrier queries cannot descend through an indexed tree
    //    (generate.rs 1993-2001 PCIT, 2105-2113 PSIT, 2209-2217 PCPSIT)
    // =================================================================

    fn assert_aggregate_carrier_rejected(db: &TempGroveDb, pq: &PathQuery, v: &GroveVersion) {
        let err = db
            .prove_query(pq, None, v)
            .unwrap()
            .expect_err("aggregate carrier through an indexed tree must be refused");
        assert!(
            matches!(err, Error::NotSupported(ref msg)
                if msg.contains("aggregate-on-range carrier queries cannot descend")
                    && msg.contains("PCIT / PSIT / PCPSIT")),
            "expected the indexed aggregate-carrier rejection, got {err:?}"
        );
    }

    #[test]
    fn aggregate_count_carrier_through_populated_pcit_rejected() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        build_pcit(&db, b"pcit", &[b"a", b"b", b"c"], v);

        let leaf =
            Query::new_aggregate_count_on_range(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        let pq = aggregate_carrier_query(b"pcit", leaf);
        assert_aggregate_carrier_rejected(&db, &pq, v);
    }

    #[test]
    fn aggregate_sum_carrier_through_populated_psit_rejected() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        build_psit(&db, b"psit", &[(b"a", 5), (b"b", 6)], v);

        let leaf =
            Query::new_aggregate_sum_on_range(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        let pq = aggregate_carrier_query(b"psit", leaf);
        assert_aggregate_carrier_rejected(&db, &pq, v);
    }

    #[test]
    fn aggregate_count_and_sum_carrier_through_populated_pcpsit_rejected() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        build_pcpsit(&db, b"pcpsit", &all_axis_tags(), &[(b"a", 5), (b"b", 6)], v);

        let leaf = Query::new_aggregate_count_and_sum_on_range(QueryItem::Range(
            b"a".to_vec()..b"z".to_vec(),
        ));
        let pq = aggregate_carrier_query(b"pcpsit", leaf);
        assert_aggregate_carrier_rejected(&db, &pq, v);
    }

    #[test]
    fn aggregate_count_carrier_through_populated_psit_and_pcpsit_rejected() {
        // Every indexed descent arm checks all three carrier flags, so an
        // ACOR carrier is refused by the PSIT and PCPSIT arms too.
        let v = GroveVersion::latest();

        let db = make_test_grovedb(v);
        build_psit(&db, b"psit", &[(b"a", 1)], v);
        let leaf =
            Query::new_aggregate_count_on_range(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        assert_aggregate_carrier_rejected(&db, &aggregate_carrier_query(b"psit", leaf), v);

        let db2 = make_test_grovedb(v);
        build_pcpsit(&db2, b"pcpsit", &[IndexAxis::Count.tag()], &[(b"a", 1)], v);
        let leaf2 =
            Query::new_aggregate_count_on_range(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        assert_aggregate_carrier_rejected(&db2, &aggregate_carrier_query(b"pcpsit", leaf2), v);
    }

    // =================================================================
    // 4. Indexed descent with a limit: the lower layer consumes the
    //    shared `overall_limit`, which flips `has_a_result_at_level`
    //    (generate.rs 2071 PCIT, 2181 PSIT, 2301 PCPSIT)
    // =================================================================

    #[test]
    fn pcit_descent_with_limit_consumes_overall_limit() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        build_pcit(&db, b"pcit", &[b"a", b"b", b"c"], v);

        // limit == row count: the descent consumes every slot, so the
        // parent layer sees `previous_limit != *overall_limit`.
        assert_eq!(
            assert_proof_round_trips(&db, &key_subquery(b"pcit", Some(3)), v),
            3
        );
        // A tighter limit truncates identically in the proof and in
        // query_raw.
        assert_eq!(
            assert_proof_round_trips(&db, &key_subquery(b"pcit", Some(2)), v),
            2
        );
    }

    #[test]
    fn psit_descent_with_limit_consumes_overall_limit() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        build_psit(&db, b"psit", &[(b"a", 1), (b"b", -4), (b"c", 9)], v);

        assert_eq!(
            assert_proof_round_trips(&db, &key_subquery(b"psit", Some(3)), v),
            3
        );
        assert_eq!(
            assert_proof_round_trips(&db, &key_subquery(b"psit", Some(2)), v),
            2
        );
    }

    #[test]
    fn pcpsit_descent_with_limit_consumes_overall_limit() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        build_pcpsit(
            &db,
            b"pcpsit",
            &all_axis_tags(),
            &[(b"a", 10), (b"b", -3), (b"c", 7)],
            v,
        );

        assert_eq!(
            assert_proof_round_trips(&db, &key_subquery(b"pcpsit", Some(3)), v),
            3
        );
        assert_eq!(
            assert_proof_round_trips(&db, &key_subquery(b"pcpsit", Some(2)), v),
            2
        );
    }
}
