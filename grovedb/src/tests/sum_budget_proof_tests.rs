//! End-to-end tests for sum-budget windows in the GroveDBProof V1
//! envelope (`ProofBytes::SumBudgetWindow`): round trips for every stop
//! condition, read/verify agreement, skip semantics, forgeries, and the
//! GROVE_V4 gates.

#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::query::query_item::QueryItem;
    use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

    use crate::{
        operations::proof::{
            GroveDBProof, LayerProof, ProofBytes, SumBudgetStop, SumBudgetWindowProof,
            VerifiedPathQuery,
        },
        tests::{make_test_sum_tree_grovedb, TEST_LEAF},
        Element, Error, GroveDb, PathQuery,
    };

    // -----------------------------------------------------------------
    // Fixtures: TEST_LEAF is a sum tree; keys a..f with mixed values.
    // -----------------------------------------------------------------

    const SUM_ENTRIES: &[(&[u8], i64)] = &[
        (b"a", 7),
        (b"b", 5),
        (b"c", -3),
        (b"d", 11),
        (b"e", 2),
        (b"f", 40),
    ];

    fn build_sum_tree(db: &GroveDb, grove_version: &GroveVersion) {
        for (key, sum) in SUM_ENTRIES {
            db.insert(
                [TEST_LEAF].as_ref(),
                key,
                Element::new_sum_item(*sum),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert sum item");
        }
    }

    fn budget_query(sum_limit: u64, match_limit: Option<u16>) -> PathQuery {
        PathQuery::new_sum_budget(
            vec![TEST_LEAF.to_vec()],
            vec![QueryItem::RangeFull(..)],
            true,
            sum_limit,
            match_limit,
        )
    }

    fn root_hash(db: &GroveDb, grove_version: &GroveVersion) -> [u8; 32] {
        db.root_hash(None, grove_version).unwrap().expect("root")
    }

    fn prove(db: &GroveDb, path_query: &PathQuery, grove_version: &GroveVersion) -> Vec<u8> {
        db.prove_query(path_query, None, grove_version)
            .unwrap()
            .expect("prove sum-budget query")
    }

    fn verify_budget(
        proof: &[u8],
        path_query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> ([u8; 32], Vec<(Vec<u8>, i64)>, i64, SumBudgetStop) {
        match GroveDb::verify_path_query(proof, path_query, grove_version)
            .expect("sum-budget proof must verify")
        {
            VerifiedPathQuery::SumBudget {
                root_hash,
                matches,
                total,
                stop,
            } => (root_hash, matches, total, stop),
            other => panic!("expected SumBudget, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // Round trips per stop condition
    // -----------------------------------------------------------------

    #[test]
    fn budget_stop_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);
        build_sum_tree(&db, grove_version);

        // Budget 10: a(7) leaves 3, b(5) drives it to -2 → stop after b.
        // (c and beyond never scanned.)
        let pq = budget_query(10, None);
        let (verified_root, matches, total, stop) =
            verify_budget(&prove(&db, &pq, grove_version), &pq, grove_version);
        assert_eq!(verified_root, root_hash(&db, grove_version));
        assert_eq!(stop, SumBudgetStop::BudgetReached);
        assert_eq!(total, 12);
        assert_eq!(
            matches,
            vec![(b"a".to_vec(), 7), (b"b".to_vec(), 5)],
            "budget stop must fire exactly after b"
        );
    }

    #[test]
    fn negative_values_give_budget_back() {
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);
        build_sum_tree(&db, grove_version);

        // Budget 13: a(7)+b(5)=12 leaves 1; c(-3) gives budget back
        // (remaining 4); d(11) drives it to -7 → stop after d.
        let pq = budget_query(13, None);
        let (_, matches, total, stop) =
            verify_budget(&prove(&db, &pq, grove_version), &pq, grove_version);
        assert_eq!(stop, SumBudgetStop::BudgetReached);
        assert_eq!(matches.len(), 4);
        assert_eq!(total, 20);
    }

    #[test]
    fn match_limit_stop_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);
        build_sum_tree(&db, grove_version);

        // Huge budget, match limit 3 → stop after c.
        let pq = budget_query(1_000_000, Some(3));
        let (_, matches, total, stop) =
            verify_budget(&prove(&db, &pq, grove_version), &pq, grove_version);
        assert_eq!(stop, SumBudgetStop::MatchLimitReached);
        assert_eq!(matches.len(), 3);
        assert_eq!(total, 9);
    }

    #[test]
    fn exhaustion_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);
        build_sum_tree(&db, grove_version);

        // Budget larger than the whole tree's net sum (62) → exhausted.
        let pq = budget_query(1_000_000, None);
        let (_, matches, total, stop) =
            verify_budget(&prove(&db, &pq, grove_version), &pq, grove_version);
        assert_eq!(stop, SumBudgetStop::Exhausted);
        assert_eq!(matches.len(), SUM_ENTRIES.len());
        assert_eq!(total, 62);
    }

    // -----------------------------------------------------------------
    // Read / verify agreement (incl. skip semantics)
    // -----------------------------------------------------------------

    #[test]
    fn verified_matches_equal_the_trusted_read() {
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);
        build_sum_tree(&db, grove_version);

        for (sum_limit, match_limit) in [(10u64, None), (13, None), (1_000_000, Some(3u16))] {
            let pq = budget_query(sum_limit, match_limit);
            let run = db
                .run_path_query(
                    &pq,
                    true,
                    true,
                    true,
                    crate::query_result_type::QueryResultType::QueryKeyElementPairResultType,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("trusted read");
            let crate::operations::get::PathQueryRun::SumBudget(read) = run else {
                panic!("expected SumBudget run");
            };
            let (_, matches, _, _) =
                verify_budget(&prove(&db, &pq, grove_version), &pq, grove_version);
            assert_eq!(
                matches, read.results,
                "sum_limit={sum_limit} match_limit={match_limit:?}: read and verified matches \
                 must agree"
            );
        }
    }

    #[test]
    fn non_sum_elements_are_scanned_and_skipped_identically() {
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);
        // a(7), then a plain item (skipped), then b(5).
        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::new_sum_item(7),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert a");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"aa",
            Element::new_item(b"not a sum".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert plain item");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"b",
            Element::new_sum_item(5),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert b");

        // Budget 10 → a(7) leaves 3, the plain item is scanned and
        // skipped, b(5) drives it negative → stop after b, two matches.
        let pq = budget_query(10, None);
        let (_, matches, total, stop) =
            verify_budget(&prove(&db, &pq, grove_version), &pq, grove_version);
        assert_eq!(stop, SumBudgetStop::BudgetReached);
        assert_eq!(matches, vec![(b"a".to_vec(), 7), (b"b".to_vec(), 5)]);
        assert_eq!(total, 12);
    }

    // -----------------------------------------------------------------
    // Forgeries
    // -----------------------------------------------------------------

    fn tamper_window(proof: &[u8], mutate: impl FnOnce(&mut SumBudgetWindowProof)) -> Vec<u8> {
        let config = bincode::config::standard().with_big_endian();
        let (mut decoded, _): (GroveDBProof, usize) =
            bincode::decode_from_slice(proof, config).expect("decode envelope");
        let GroveDBProof::V1(ref mut v1) = decoded else {
            panic!("expected V1 envelope");
        };
        fn find_window(layer: &mut LayerProof) -> Option<&mut LayerProof> {
            if matches!(layer.merk_proof, ProofBytes::SumBudgetWindow(_)) {
                return Some(layer);
            }
            layer.lower_layers.values_mut().find_map(find_window)
        }
        let window = find_window(&mut v1.root_layer).expect("envelope has a sum-budget window");
        let ProofBytes::SumBudgetWindow(bytes) = &window.merk_proof else {
            unreachable!();
        };
        let mut payload = SumBudgetWindowProof::decode_canonical(bytes).expect("decode payload");
        mutate(&mut payload);
        window.merk_proof =
            ProofBytes::SumBudgetWindow(payload.encode_canonical().expect("re-encode"));
        bincode::encode_to_vec(&decoded, config).expect("re-encode envelope")
    }

    #[test]
    fn understating_the_window_is_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);
        build_sum_tree(&db, grove_version);
        let pq = budget_query(10, None);
        let proof = prove(&db, &pq, grove_version);
        GroveDb::verify_path_query(&proof, &pq, grove_version).expect("honest verifies");

        // Claim the walk stopped one element earlier: the replay finds
        // no stop condition fired within the shortened window.
        let tampered = tamper_window(&proof, |payload| {
            payload.window_len -= 1;
        });
        GroveDb::verify_path_query(&tampered, &pq, grove_version)
            .expect_err("an understated window must be rejected");
    }

    #[test]
    fn lying_about_exhaustion_is_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);
        build_sum_tree(&db, grove_version);

        // Budget stops after b; claiming exhaustion must fail (the
        // replay sees the budget fire inside the window).
        let pq = budget_query(10, None);
        let proof = prove(&db, &pq, grove_version);
        let tampered = tamper_window(&proof, |payload| {
            payload.exhausted = true;
        });
        GroveDb::verify_path_query(&tampered, &pq, grove_version)
            .expect_err("claiming exhaustion over a budget stop must be rejected");

        // Conversely: a genuinely exhausted walk claiming a stop.
        let pq = budget_query(1_000_000, None);
        let proof = prove(&db, &pq, grove_version);
        let tampered = tamper_window(&proof, |payload| {
            payload.exhausted = false;
        });
        GroveDb::verify_path_query(&tampered, &pq, grove_version)
            .expect_err("claiming a stop over an exhausted walk must be rejected");
    }

    #[test]
    fn plain_descent_at_a_sum_budget_position_is_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);
        build_sum_tree(&db, grove_version);

        // Prove the same window as a PLAIN key-selection query, then
        // verify against the sum-budget query: the walk must reject the
        // Merk layer where the window envelope is required.
        let plain = PathQuery::new_unsized(
            vec![TEST_LEAF.to_vec()],
            grovedb_merk::proofs::Query::new_single_query_item(QueryItem::RangeFull(..)),
        );
        let plain_proof = prove(&db, &plain, grove_version);
        let pq = budget_query(10, None);
        GroveDb::verify_path_query(&plain_proof, &pq, grove_version)
            .expect_err("a plain descent at a sum-budget position must be rejected");
    }

    // -----------------------------------------------------------------
    // Gates
    // -----------------------------------------------------------------

    #[test]
    fn sum_budget_windows_are_gated_to_grove_v4_on_both_sides() {
        let v3 = &GROVE_VERSIONS[2];
        assert_eq!(v3.protocol_version, 3);
        let v4 = GroveVersion::latest();

        let db = make_test_sum_tree_grovedb(v4);
        build_sum_tree(&db, v4);
        let pq = budget_query(10, None);

        match db.prove_query(&pq, None, v3).unwrap() {
            Err(Error::NotSupported(_)) => {}
            other => panic!("V3 prover must refuse sum-budget shapes, got {other:?}"),
        }

        let proof = prove(&db, &pq, v4);
        match GroveDb::verify_path_query(&proof, &pq, v3) {
            Err(Error::NotSupported(_)) => {}
            other => panic!("V3 verifier must reject sum-budget shapes, got {other:?}"),
        }
    }

    /// `SumItemWithBackwardsReferences` passes the verifier's sum-item
    /// check, so its value must also be extracted by the sum fold —
    /// previously the extraction match missed the variant and rejected an
    /// honest proof as invalid.
    #[test]
    fn budget_window_includes_backward_references_sum_items() {
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);
        for (key, sum) in [(b"a".as_ref(), 7i64), (b"b", 5), (b"c", 11)] {
            db.insert(
                [TEST_LEAF].as_ref(),
                key,
                Element::new_sum_item_allowing_bidirectional_references(sum),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert backward-references sum item");
        }

        let pq = budget_query(20, None);
        let (proved_root, matches, consumed, stop) =
            verify_budget(&prove(&db, &pq, grove_version), &pq, grove_version);

        assert_eq!(proved_root, root_hash(&db, grove_version));
        assert_eq!(stop, SumBudgetStop::BudgetReached);
        // 7 + 5 + 11 crosses the 20 budget on the third element.
        assert_eq!(consumed, 23);
        assert_eq!(
            matches,
            vec![(b"a".to_vec(), 7), (b"b".to_vec(), 5), (b"c".to_vec(), 11)]
        );
    }
}
