//! End-to-end GroveDB tests for `AggregateSumOnRange` queries.
//!
//! These exercise the full prove → encode → decode → verify pipeline against
//! `ProvableSumTree` at various path depths and across the full set of
//! allowed range variants. Mirrors `aggregate_count_query_tests.rs` for the
//! signed-sum flavor, with extra cases covering negative sums, mixed signs
//! at i64 extremes, and the i128-accumulator overflow gate.

#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::query::QueryItem;
    use grovedb_version::version::{v2::GROVE_V2, GroveVersion};

    use crate::{
        tests::{make_test_grovedb, TEST_LEAF},
        Element, GroveDb, PathQuery,
    };

    /// Insert keys "a".."o" (15 keys) into a `ProvableSumTree` rooted at
    /// `[TEST_LEAF, "st"]`, with sums 1..=15. Returns the db and root hash.
    fn setup_15_key_provable_sum_tree(
        grove_version: &GroveVersion,
    ) -> (crate::tests::TempGroveDb, [u8; 32]) {
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"st",
            Element::empty_provable_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert st");
        for (i, c) in (b'a'..=b'o').enumerate() {
            let value = (i as i64) + 1;
            db.insert(
                [TEST_LEAF, b"st"].as_ref(),
                &[c],
                Element::new_sum_item(value),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert sum item");
        }
        let root = db
            .grove_db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root_hash");
        (db, root)
    }

    /// Round-trip: build a PathQuery, prove it, verify it, assert
    /// `(root, sum)` matches.
    fn round_trip(
        db: &crate::tests::TempGroveDb,
        expected_root: [u8; 32],
        path: Vec<Vec<u8>>,
        inner_range: QueryItem,
        expected_sum: i64,
        grove_version: &GroveVersion,
    ) {
        let path_query = PathQuery::new_aggregate_sum_on_range(path, inner_range);
        let proof = db
            .grove_db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove_query should succeed");
        let (root, sum) = GroveDb::verify_aggregate_sum_query(&proof, &path_query, grove_version)
            .expect("verify should succeed");
        assert_eq!(root, expected_root, "verifier reconstructed wrong root");
        assert_eq!(sum, expected_sum, "verifier returned wrong sum");
    }

    // ---------- 1. Round-trip: single-key sum tree ----------
    /// A `ProvableSumTree` with just one key: the proof should still
    /// reconstruct correctly, and the sum should be the single value.
    /// (Empty-tree round-trip at the GroveDB-envelope level is covered by
    /// the merk-side `integration_empty_merk_sum` test — at GroveDB level
    /// an empty subtree produces no `lower_layers` entry, which is a
    /// separate routing concern from the proof shape we're testing here.)
    #[test]
    fn single_key_provable_sum_tree_round_trip() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"st",
            Element::empty_provable_sum_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert st");
        db.insert(
            [TEST_LEAF, b"st"].as_ref(),
            b"k",
            Element::new_sum_item(42),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert single sum item");
        let root = db.grove_db.root_hash(None, v).unwrap().expect("root_hash");
        round_trip(
            &db,
            root,
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
            42,
            v,
        );
    }

    // ---------- 2. Round-trip full range: sum 1+2+...+15 = 120 ----------
    #[test]
    fn provable_sum_tree_full_range_from() {
        let v = GroveVersion::latest();
        let (db, root) = setup_15_key_provable_sum_tree(v);
        round_trip(
            &db,
            root,
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::RangeFrom(b"a".to_vec()..),
            120,
            v,
        );
    }

    // ---------- 3. Subrange: c..=l (values 3..=12) → 75 ----------
    #[test]
    fn provable_sum_tree_range_inclusive() {
        let v = GroveVersion::latest();
        let (db, root) = setup_15_key_provable_sum_tree(v);
        round_trip(
            &db,
            root,
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
            75,
            v,
        );
    }

    // ---------- 3b. RangeAfter ----------
    #[test]
    fn provable_sum_tree_range_after() {
        let v = GroveVersion::latest();
        let (db, root) = setup_15_key_provable_sum_tree(v);
        // RangeAfter("b") matches c..o → 3+4+...+15 = 117.
        round_trip(
            &db,
            root,
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::RangeAfter(b"b".to_vec()..),
            117,
            v,
        );
    }

    // ---------- 3c. RangeToInclusive ----------
    #[test]
    fn provable_sum_tree_range_to_inclusive() {
        let v = GroveVersion::latest();
        let (db, root) = setup_15_key_provable_sum_tree(v);
        // ..=e → 1+2+3+4+5 = 15.
        round_trip(
            &db,
            root,
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::RangeToInclusive(..=b"e".to_vec()),
            15,
            v,
        );
    }

    // ---------- 4. Boundary: range [b"c"..=b"c"] → 3 ----------
    #[test]
    fn provable_sum_tree_single_key_range() {
        let v = GroveVersion::latest();
        let (db, root) = setup_15_key_provable_sum_tree(v);
        round_trip(
            &db,
            root,
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"c".to_vec()),
            3,
            v,
        );
    }

    // ---------- 5. Negative sums: mixed +/- children → net -70 ----------
    #[test]
    fn provable_sum_tree_negative_sums_mixed() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"st",
            Element::empty_provable_sum_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert st");
        // Sums: +50, -100, +30, -50 → net -70.
        let entries: [(u8, i64); 4] = [(b'a', 50), (b'b', -100), (b'c', 30), (b'd', -50)];
        for (k, val) in entries {
            db.insert(
                [TEST_LEAF, b"st"].as_ref(),
                &[k],
                Element::new_sum_item(val),
                None,
                None,
                v,
            )
            .unwrap()
            .expect("insert sum item");
        }
        let root = db.grove_db.root_hash(None, v).unwrap().expect("root_hash");
        round_trip(
            &db,
            root,
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::RangeInclusive(b"a".to_vec()..=b"d".to_vec()),
            -70,
            v,
        );
    }

    // ---------- 5b. Negative-only: subrange contains only negatives ----------
    #[test]
    fn provable_sum_tree_all_negative_subrange() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"st",
            Element::empty_provable_sum_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert st");
        let entries: [(u8, i64); 4] = [(b'a', 5), (b'b', -3), (b'c', -7), (b'd', 8)];
        for (k, val) in entries {
            db.insert(
                [TEST_LEAF, b"st"].as_ref(),
                &[k],
                Element::new_sum_item(val),
                None,
                None,
                v,
            )
            .unwrap()
            .expect("insert sum item");
        }
        let root = db.grove_db.root_hash(None, v).unwrap().expect("root_hash");
        // Range b..=c → -3 + -7 = -10.
        round_trip(
            &db,
            root,
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::RangeInclusive(b"b".to_vec()..=b"c".to_vec()),
            -10,
            v,
        );
    }

    // ---------- 5c. Plus and minus cancel to zero (NOT a short-circuit case) ----------
    /// Sum can legitimately be zero with non-zero children. The verifier
    /// must produce 0 by genuine arithmetic, not by any "if sum == 0 →
    /// skip" shortcut (a bug the count code can use but the sum code can't).
    #[test]
    fn provable_sum_tree_sum_zero_from_offsetting_children() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"st",
            Element::empty_provable_sum_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert st");
        for (k, val) in [(b'a', 5i64), (b'b', -5i64)] {
            db.insert(
                [TEST_LEAF, b"st"].as_ref(),
                &[k],
                Element::new_sum_item(val),
                None,
                None,
                v,
            )
            .unwrap()
            .expect("insert sum item");
        }
        let root = db.grove_db.root_hash(None, v).unwrap().expect("root_hash");
        round_trip(
            &db,
            root,
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::RangeInclusive(b"a".to_vec()..=b"b".to_vec()),
            0,
            v,
        );
    }

    // ---------- 6. i64::MAX + i64::MAX → verify returns overflow error ----------
    /// Two i64::MAX children sum to 2*i64::MAX which doesn't fit in i64.
    /// The verifier's final i64-narrowing check must reject. Whether the
    /// underlying tree allows insertion depends on Phase 1's intermediate-
    /// overflow handling — if it doesn't, we exit early; the merk-side
    /// test in `merk::aggregate_sum::integration_overflow_at_i64_max_is_rejected`
    /// additionally exercises this via a directly-fabricated proof.
    #[test]
    fn provable_sum_tree_overflow_at_i64_max_is_rejected() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"st",
            Element::empty_provable_sum_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert st");
        // First i64::MAX inserts cleanly. The second insert may or may not
        // succeed depending on aggregation overflow rules — accept either,
        // we only require that an *eventual* proof+verify can't silently
        // produce a wrong i64.
        let ok1 = db
            .insert(
                [TEST_LEAF, b"st"].as_ref(),
                b"a",
                Element::new_sum_item(i64::MAX),
                None,
                None,
                v,
            )
            .unwrap()
            .is_ok();
        let ok2 = db
            .insert(
                [TEST_LEAF, b"st"].as_ref(),
                b"b",
                Element::new_sum_item(i64::MAX),
                None,
                None,
                v,
            )
            .unwrap()
            .is_ok();
        if !ok1 || !ok2 {
            // Insertion already rejected the overflow — that's the
            // healthiest end state. Bail out.
            return;
        }
        let pq = PathQuery::new_aggregate_sum_on_range(
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::RangeInclusive(b"a".to_vec()..=b"b".to_vec()),
        );
        let prove_result = db.grove_db.prove_query(&pq, None, v).unwrap();
        match prove_result {
            Err(_) => { /* prover detected overflow — fine */ }
            Ok(proof) => {
                let verify_result = GroveDb::verify_aggregate_sum_query(&proof, &pq, v);
                assert!(
                    verify_result.is_err(),
                    "verifier must reject a sum that doesn't fit in i64"
                );
            }
        }
    }

    // ---------- 7. i64::MAX + i64::MIN = -1 (intermediate overflows i64 but final fits) ----------
    #[test]
    fn provable_sum_tree_mixed_extremes_sum_to_negative_one() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"st",
            Element::empty_provable_sum_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert st");
        // i64::MAX + i64::MIN. In i128 the intermediate computes to -1
        // cleanly; in i64 it would overflow if computed naively. The
        // verifier uses i128 throughout, so it must reach -1.
        let ok1 = db
            .insert(
                [TEST_LEAF, b"st"].as_ref(),
                b"a",
                Element::new_sum_item(i64::MAX),
                None,
                None,
                v,
            )
            .unwrap()
            .is_ok();
        let ok2 = db
            .insert(
                [TEST_LEAF, b"st"].as_ref(),
                b"b",
                Element::new_sum_item(i64::MIN),
                None,
                None,
                v,
            )
            .unwrap()
            .is_ok();
        if !ok1 || !ok2 {
            return; // tree-level overflow detection; not our scenario today
        }
        let root = db.grove_db.root_hash(None, v).unwrap().expect("root_hash");
        // Cumulative aggregate at the tree level should already be -1 if
        // both inserts succeeded. The range covering both should report
        // -1, not panic or wrap.
        round_trip(
            &db,
            root,
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::RangeInclusive(b"a".to_vec()..=b"b".to_vec()),
            -1,
            v,
        );
    }

    // ---------- 8. Tampering: mutate HashWithSum's sum field ----------
    #[test]
    fn tampered_hash_with_sum_byte_is_rejected() {
        let v = GroveVersion::latest();
        let (db, _root) = setup_15_key_provable_sum_tree(v);
        let pq = PathQuery::new_aggregate_sum_on_range(
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
        );
        let mut proof = db
            .grove_db
            .prove_query(&pq, None, v)
            .unwrap()
            .expect("prove");
        // Flip a byte deep enough that it lands inside the leaf merk proof
        // (past the envelope's metadata).
        let target = proof.len() / 2;
        proof[target] = proof[target].wrapping_add(1);
        let result = GroveDb::verify_aggregate_sum_query(&proof, &pq, v);
        assert!(
            result.is_err(),
            "tampered proof byte must be rejected, got {:?}",
            result.map(|(_, s)| s)
        );
    }

    // ---------- 9. Tampering: mutate KVSum's sum field (via byte flip) ----------
    /// The proof envelope contains both `HashWithSum` and `KVDigestSum`
    /// nodes; flipping any byte that lands inside their sum encoding
    /// must be caught by the chain check. We try several positions to
    /// raise the probability of hitting a sum byte.
    #[test]
    fn multiple_byte_flips_in_leaf_are_all_rejected() {
        let v = GroveVersion::latest();
        let (db, _root) = setup_15_key_provable_sum_tree(v);
        let pq = PathQuery::new_aggregate_sum_on_range(
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
        );
        let honest = db
            .grove_db
            .prove_query(&pq, None, v)
            .unwrap()
            .expect("prove");

        // Try a handful of bytes in the back half of the proof (the leaf
        // merk bytes). For each, the verifier must either error or return
        // a different root hash than honest. We deliberately do not pin
        // exact byte indices so this test stays robust against encoding
        // tweaks.
        let honest_decoded =
            GroveDb::verify_aggregate_sum_query(&honest, &pq, v).expect("honest verify");
        let mut at_least_one_caught = false;
        for offset_frac in [3, 5, 7, 9] {
            let target = honest.len() * offset_frac / 10;
            if target >= honest.len() {
                continue;
            }
            let mut bytes = honest.clone();
            bytes[target] = bytes[target].wrapping_add(0x5a);
            match GroveDb::verify_aggregate_sum_query(&bytes, &pq, v) {
                Err(_) => at_least_one_caught = true,
                Ok((root, _sum)) if root != honest_decoded.0 => at_least_one_caught = true,
                Ok(_) => {
                    // Same (root, sum) is acceptable — the byte didn't
                    // change the semantic outcome (e.g. a length-prefix
                    // padding bit). Keep trying.
                }
            }
        }
        assert!(
            at_least_one_caught,
            "at least one of several leaf byte flips should have been caught"
        );
    }

    // ---------- 10. Wrong path: returns root ≠ trusted root ----------
    /// If a caller verifies against a proof for a *different* tree, the
    /// returned root won't match their trusted root and the application
    /// rejects on that comparison. The verifier itself doesn't take a
    /// trusted root; it returns the reconstructed one for the caller to
    /// compare. We assert the returned root differs from what an
    /// unrelated tree would produce.
    #[test]
    fn proof_for_different_tree_yields_different_root() {
        let v = GroveVersion::latest();
        let (db1, root1) = setup_15_key_provable_sum_tree(v);
        // Build a *different* db with the same path shape but different
        // values, generate a proof against it, and confirm that proof
        // verifies to root2 ≠ root1.
        let db2 = make_test_grovedb(v);
        db2.insert(
            [TEST_LEAF].as_ref(),
            b"st",
            Element::empty_provable_sum_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert");
        db2.insert(
            [TEST_LEAF, b"st"].as_ref(),
            b"a",
            Element::new_sum_item(999),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert");
        let root2 = db2.grove_db.root_hash(None, v).unwrap().expect("root2");
        assert_ne!(root1, root2);

        let pq = PathQuery::new_aggregate_sum_on_range(
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::RangeFrom(b"a".to_vec()..),
        );
        let proof2 = db2
            .grove_db
            .prove_query(&pq, None, v)
            .unwrap()
            .expect("prove2");
        let (got_root, _sum) =
            GroveDb::verify_aggregate_sum_query(&proof2, &pq, v).expect("verify against db2 proof");
        assert_eq!(got_root, root2);
        assert_ne!(
            got_root, root1,
            "caller's root check must catch wrong-tree proofs"
        );
    }

    // ---------- 11. Wrong query shape: PathQuery with subquery is rejected ----------
    #[test]
    fn aggregate_sum_with_subquery_is_rejected_at_validation() {
        let v = GroveVersion::latest();
        let mut pq = PathQuery::new_aggregate_sum_on_range(
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
        );
        // Sneak in a subquery — the validator must reject on the
        // verifier side.
        pq.query
            .query
            .set_subquery(grovedb_merk::proofs::Query::new_range_full());
        let dummy_proof = vec![0u8; 16];
        assert!(GroveDb::verify_aggregate_sum_query(&dummy_proof, &pq, v).is_err());

        // Defense-in-depth: the *prover* must also refuse a malformed
        // ASOR path query. Without this assertion a regression in
        // `prove_query_non_serialized` could silently produce a proof
        // for a malformed shape while the verifier-side test still
        // passed on the dummy bytes. We need an actual db to call
        // prove_query; reuse the standard 15-key fixture.
        let (db, _root) = setup_15_key_provable_sum_tree(v);
        let prove_result = db.grove_db.prove_query(&pq, None, v).unwrap();
        assert!(
            prove_result.is_err(),
            "prover must refuse to run ASOR with a hidden subquery, got Ok"
        );
    }

    // ---------- 12. Empty range (start > end is structurally invalid; use range above all keys → 0) ----------
    #[test]
    fn range_above_all_keys_returns_zero_sum() {
        let v = GroveVersion::latest();
        let (db, root) = setup_15_key_provable_sum_tree(v);
        round_trip(
            &db,
            root,
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::RangeInclusive(b"z".to_vec()..=vec![0xff]),
            0,
            v,
        );
    }

    // ---------- 12b. Range below all keys → 0 ----------
    #[test]
    fn range_below_all_keys_returns_zero_sum() {
        let v = GroveVersion::latest();
        let (db, root) = setup_15_key_provable_sum_tree(v);
        round_trip(
            &db,
            root,
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::RangeInclusive(vec![0x00]..=vec![0x10]),
            0,
            v,
        );
    }

    // ---------- 13. Multi-layer path (3 layers) ----------
    /// Outer NormalTree → inner ProvableSumTree. Exercises the chain
    /// enforcement that count tests use, with sum semantics.
    fn setup_three_layer_provable_sum_tree(
        grove_version: &GroveVersion,
    ) -> (crate::tests::TempGroveDb, [u8; 32]) {
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert outer");
        db.insert(
            [TEST_LEAF, b"outer"].as_ref(),
            b"inner",
            Element::empty_provable_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert inner");
        // Five keys: a..=e with values 1..=5; sum = 15.
        for (i, c) in (b'a'..=b'e').enumerate() {
            db.insert(
                [TEST_LEAF, b"outer", b"inner"].as_ref(),
                &[c],
                Element::new_sum_item((i as i64) + 1),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert sum item");
        }
        let root = db
            .grove_db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root_hash");
        (db, root)
    }

    #[test]
    fn three_layer_path_round_trip_sum() {
        let v = GroveVersion::latest();
        let (db, root) = setup_three_layer_provable_sum_tree(v);
        // RangeInclusive("b"..="d") matches values 2+3+4 = 9.
        round_trip(
            &db,
            root,
            vec![TEST_LEAF.to_vec(), b"outer".to_vec(), b"inner".to_vec()],
            QueryItem::RangeInclusive(b"b".to_vec()..=b"d".to_vec()),
            9,
            v,
        );
    }

    // ---------- 14. Illegal mix: AggregateSumOnRange + AggregateCountOnRange ----------
    /// Constructing a `PathQuery` that contains both aggregate variants is
    /// possible at the Vec level, but validation must reject — the two
    /// types are explicitly orthogonal.
    #[test]
    fn mixed_aggregate_sum_and_count_is_rejected() {
        let v = GroveVersion::latest();
        let mut pq = PathQuery::new_aggregate_sum_on_range(
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
        );
        // Manually push an AggregateCountOnRange — the surrounding query
        // now has two items, which validation rejects ("must be the only
        // item").
        pq.query
            .query
            .items
            .push(QueryItem::AggregateCountOnRange(Box::new(
                QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
            )));
        let dummy_proof = vec![0u8; 16];
        let err = GroveDb::verify_aggregate_sum_query(&dummy_proof, &pq, v)
            .expect_err("mixed aggregates must be rejected");
        let msg = format!("{:?}", err);
        assert!(
            msg.contains("only item") || msg.contains("InvalidQuery"),
            "expected validation rejection, got: {msg}"
        );
    }

    // ---------- 15. Validation: nested AggregateSumOnRange ----------
    #[test]
    fn validate_at_construction_rejects_nested_aggregate_sum_on_range() {
        let pq = PathQuery::new_aggregate_sum_on_range(
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::AggregateSumOnRange(Box::new(QueryItem::Range(
                b"a".to_vec()..b"z".to_vec(),
            ))),
        );
        assert!(pq.validate_aggregate_sum_on_range().is_err());
    }

    // ---------- 16. Validation: AggregateSumOnRange wrapping AggregateCountOnRange ----------
    #[test]
    fn validate_rejects_sum_wrapping_count() {
        let pq = PathQuery::new_aggregate_sum_on_range(
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::AggregateCountOnRange(Box::new(QueryItem::Range(
                b"a".to_vec()..b"z".to_vec(),
            ))),
        );
        assert!(pq.validate_aggregate_sum_on_range().is_err());
    }

    // ---------- 17. Validation: Key inner is rejected ----------
    #[test]
    fn validate_rejects_key_inner() {
        let pq = PathQuery::new_aggregate_sum_on_range(
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::Key(b"a".to_vec()),
        );
        assert!(pq.validate_aggregate_sum_on_range().is_err());
    }

    // ---------- 18. Validation: RangeFull inner is rejected ----------
    #[test]
    fn validate_rejects_range_full_inner() {
        let pq = PathQuery::new_aggregate_sum_on_range(
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::RangeFull(std::ops::RangeFull),
        );
        assert!(pq.validate_aggregate_sum_on_range().is_err());
    }

    // ---------- 19. Rejected on non-ProvableSumTree (NormalTree) ----------
    #[test]
    fn proof_rejected_on_normal_tree_path() {
        // The path points to a normal tree, not a ProvableSumTree. The
        // prover must refuse — either at the merk-level tree-type gate
        // (`prove_aggregate_sum_on_range` errors on non-ProvableSumTree)
        // or, if the prover happens to produce some bytes, the verifier
        // must reject during the leaf-level shape walk because the proof
        // ops won't be the sum-flavor variants the verifier expects.
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"normal",
            Element::empty_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert normal");
        // Add a child so the subtree isn't empty — empty subtrees can
        // short-circuit in places that bypass the tree-type check.
        db.insert(
            [TEST_LEAF, b"normal"].as_ref(),
            b"a",
            Element::new_item(b"v".to_vec()),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert child");
        let pq = PathQuery::new_aggregate_sum_on_range(
            vec![TEST_LEAF.to_vec(), b"normal".to_vec()],
            QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
        );
        match db.grove_db.prove_query(&pq, None, v).unwrap() {
            Err(_) => { /* prover rejected — good */ }
            Ok(proof) => {
                // Prover didn't catch it (e.g. via an unrelated path);
                // the verifier must catch it.
                let r = GroveDb::verify_aggregate_sum_query(&proof, &pq, v);
                assert!(
                    r.is_err(),
                    "verifier must reject sum proof against non-ProvableSumTree"
                );
            }
        }
    }

    // ---------- 20. V0 (GROVE_V2) envelope round-trip ----------
    #[test]
    fn provable_sum_tree_works_on_grove_v2_envelope() {
        let v: &GroveVersion = &GROVE_V2;
        let (db, root) = setup_15_key_provable_sum_tree(v);
        let pq = PathQuery::new_aggregate_sum_on_range(
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
        );
        let proof = db
            .grove_db
            .prove_query(&pq, None, v)
            .unwrap()
            .expect("prove_query (v0 envelope) should succeed");
        let (got_root, got_sum) =
            GroveDb::verify_aggregate_sum_query(&proof, &pq, v).expect("verify v0 envelope");
        assert_eq!(got_root, root);
        assert_eq!(got_sum, 75);
    }

    // ---------- 21. NotSummed-wrapped child tree contributes 0 ----------
    /// `Element::NotSummed` wraps a *sum-tree variant* and tells the parent
    /// to skip the wrapped subtree's aggregate sum. Verify the proof
    /// honors that exclusion: the wrapped subtree's sum doesn't
    /// contribute to the parent ProvableSumTree's `KVDigestSum.aggregate`,
    /// so the aggregate query at the parent level sees only the un-wrapped
    /// sum items.
    #[test]
    fn not_summed_child_tree_excluded_from_aggregate_sum() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"st",
            Element::empty_provable_sum_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert st");
        // Two regular sum items (5 + 7 = 12).
        db.insert(
            [TEST_LEAF, b"st"].as_ref(),
            b"a",
            Element::new_sum_item(5),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert a");
        db.insert(
            [TEST_LEAF, b"st"].as_ref(),
            b"b",
            Element::new_sum_item(7),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert b");
        // One NotSummed-wrapped ProvableSumTree at key "c". Its inner
        // children's sum contributes nothing to the parent's aggregate.
        let ns_tree =
            Element::new_not_summed(Element::empty_provable_sum_tree()).expect("wrap not_summed");
        db.insert([TEST_LEAF, b"st"].as_ref(), b"c", ns_tree, None, None, v)
            .unwrap()
            .expect("insert NotSummed tree");
        // Put a value inside the wrapped subtree to confirm it doesn't
        // bleed into the parent's aggregate.
        db.insert(
            [TEST_LEAF, b"st", b"c"].as_ref(),
            b"hidden",
            Element::new_sum_item(100),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert hidden");
        let root = db.grove_db.root_hash(None, v).unwrap().expect("root_hash");
        round_trip(
            &db,
            root,
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::RangeInclusive(b"a".to_vec()..=b"c".to_vec()),
            12, // NotSummed-wrapped subtree contributes 0 → 5+7 = 12
            v,
        );
    }

    // ---------- 22. Empty-leaf type-confusion forgery (security regression) -
    /// Codex security finding: when an honest tree at the queried leaf path
    /// is an empty Merk-backed tree of any non-ProvableSumTree type
    /// (NormalTree, SumTree, ProvableCountTree, …), every such tree stores
    /// `inner_root = NULL_HASH`, so its recorded value_hash equals
    /// `combine_hash(H(element_bytes), NULL_HASH)`. The merk-level sum
    /// verifier accepts empty proof bytes as `(NULL_HASH, 0)`. The
    /// pre-fix verifier's `is_any_tree()` check happily accepted those
    /// non-ProvableSumTree element bytes — and the chain-hash check
    /// passed trivially — letting an attacker prove `sum = 0` against a
    /// path that wasn't actually a ProvableSumTree. The numeric answer
    /// (0) was correct for an empty tree of any type, but the implicit
    /// claim "the leaf is a ProvableSumTree" was a soundness gap.
    ///
    /// This test surgically constructs the forged proof from a real
    /// honest single-key envelope and confirms the new
    /// terminal-type gate rejects it.
    #[test]
    fn empty_leaf_type_confusion_forgery_rejected() {
        use std::collections::BTreeMap;

        use bincode::config;

        use crate::operations::proof::{
            GroveDBProof, GroveDBProofV0, MerkOnlyLayerProof, ProveOptions,
        };

        // Use V0 (GROVE_V2) envelope — its MerkOnlyLayerProof is simpler to
        // surgically reconstruct than V1's LayerProof/ProofBytes.
        let v: &GroveVersion = &GROVE_V2;

        // Build the malicious tree state: an empty NormalTree at the path
        // we'll later claim is a ProvableSumTree. We exercise the bypass
        // on the empty case specifically — that's the only case where the
        // pre-fix chain check passes.
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"evil",
            Element::empty_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert empty normal tree at evil");

        // Run an honest "does evil exist?" single-key probe via prove_query
        // to harvest the layer-0 merk proof bytes (proves `evil` exists in
        // TEST_LEAF with its NormalTree element bytes). The result has the
        // shape we need for the layer-0 portion of the forgery.
        let probe = PathQuery::new_single_key(vec![TEST_LEAF.to_vec()], b"evil".to_vec());
        let probe_proof_bytes = db
            .grove_db
            .prove_query(&probe, None, v)
            .unwrap()
            .expect("honest probe should succeed");

        let cfg = config::standard()
            .with_big_endian()
            .with_limit::<{ 256 * 1024 * 1024 }>();
        let probe_decoded: GroveDBProof = bincode::decode_from_slice(&probe_proof_bytes, cfg)
            .unwrap()
            .0;

        // Forge a V0 envelope:
        //   root_layer.merk_proof = honest proof of TEST_LEAF in root
        //   root_layer.lower_layers[TEST_LEAF].merk_proof = honest proof of
        //                                   "evil" in TEST_LEAF
        //   root_layer.lower_layers[TEST_LEAF].lower_layers["evil"].merk_proof = []
        //                                   <-- forged empty leaf
        let (root_merk_proof_bytes, test_leaf_merk_proof_bytes) = match probe_decoded {
            GroveDBProof::V0(GroveDBProofV0 { root_layer, .. }) => {
                let test_leaf = root_layer
                    .lower_layers
                    .get(TEST_LEAF)
                    .expect("probe must descend into TEST_LEAF")
                    .merk_proof
                    .clone();
                (root_layer.merk_proof, test_leaf)
            }
            GroveDBProof::V1(_) => panic!("expected V0 envelope under GROVE_V2"),
        };

        let leaf_layer = MerkOnlyLayerProof {
            merk_proof: Vec::new(), // the forged empty leaf
            lower_layers: BTreeMap::new(),
        };
        let mut test_leaf_map = BTreeMap::new();
        test_leaf_map.insert(b"evil".to_vec(), leaf_layer);

        let test_leaf_layer = MerkOnlyLayerProof {
            merk_proof: test_leaf_merk_proof_bytes,
            lower_layers: test_leaf_map,
        };
        let mut root_lower = BTreeMap::new();
        root_lower.insert(TEST_LEAF.to_vec(), test_leaf_layer);

        let forged_envelope = GroveDBProof::V0(GroveDBProofV0 {
            root_layer: MerkOnlyLayerProof {
                merk_proof: root_merk_proof_bytes,
                lower_layers: root_lower,
            },
            prove_options: ProveOptions::default(),
        });
        let forged_bytes =
            bincode::encode_to_vec(&forged_envelope, cfg).expect("encode forged envelope");

        // The attacker submits the forged proof against an aggregate-sum
        // path that targets the empty NormalTree as if it were a
        // ProvableSumTree.
        let attack_pq = PathQuery::new_aggregate_sum_on_range(
            vec![TEST_LEAF.to_vec(), b"evil".to_vec()],
            QueryItem::RangeFrom(b"a".to_vec()..),
        );

        let result = GroveDb::verify_aggregate_sum_query(&forged_bytes, &attack_pq, v);
        match result {
            Err(e) => {
                // The new terminal-type gate must fire. The error message
                // names ProvableSumTree explicitly so we pin it.
                let msg = format!("{e}");
                assert!(
                    msg.contains("must be a ProvableSumTree") || msg.contains("ProvableSumTree"),
                    "verifier rejected as expected but with an unrelated message: {msg}"
                );
            }
            Ok((root_hash, sum)) => panic!(
                "BUG: empty-leaf forgery accepted by verifier! \
                 Returned (root_hash={}, sum={}) — the leaf is a NormalTree, \
                 not a ProvableSumTree.",
                hex::encode(root_hash),
                sum
            ),
        }
    }

    /// Same forgery shape, but the honest leaf is an empty
    /// `ProvableCountTree` (the wrong PROVABLE tree type for a sum
    /// query). Confirms the terminal-type gate enforces the precise
    /// tree-type, not just "any provable aggregate tree".
    #[test]
    fn empty_provable_count_tree_at_leaf_rejected_for_sum() {
        use std::collections::BTreeMap;

        use bincode::config;

        use crate::operations::proof::{
            GroveDBProof, GroveDBProofV0, MerkOnlyLayerProof, ProveOptions,
        };

        let v: &GroveVersion = &GROVE_V2;
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pct",
            Element::empty_provable_count_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert empty provable count tree");

        let probe = PathQuery::new_single_key(vec![TEST_LEAF.to_vec()], b"pct".to_vec());
        let probe_proof_bytes = db
            .grove_db
            .prove_query(&probe, None, v)
            .unwrap()
            .expect("honest probe");
        let cfg = config::standard()
            .with_big_endian()
            .with_limit::<{ 256 * 1024 * 1024 }>();
        let probe_decoded: GroveDBProof = bincode::decode_from_slice(&probe_proof_bytes, cfg)
            .unwrap()
            .0;

        let (root_mp, test_leaf_mp) = match probe_decoded {
            GroveDBProof::V0(GroveDBProofV0 { root_layer, .. }) => (
                root_layer.merk_proof,
                root_layer
                    .lower_layers
                    .get(TEST_LEAF)
                    .expect("descent")
                    .merk_proof
                    .clone(),
            ),
            GroveDBProof::V1(_) => panic!("expected V0"),
        };

        let mut leaf = BTreeMap::new();
        leaf.insert(
            b"pct".to_vec(),
            MerkOnlyLayerProof {
                merk_proof: Vec::new(),
                lower_layers: BTreeMap::new(),
            },
        );
        let mut root_lower = BTreeMap::new();
        root_lower.insert(
            TEST_LEAF.to_vec(),
            MerkOnlyLayerProof {
                merk_proof: test_leaf_mp,
                lower_layers: leaf,
            },
        );
        let forged = GroveDBProof::V0(GroveDBProofV0 {
            root_layer: MerkOnlyLayerProof {
                merk_proof: root_mp,
                lower_layers: root_lower,
            },
            prove_options: ProveOptions::default(),
        });
        let forged_bytes = bincode::encode_to_vec(&forged, cfg).expect("encode");

        let attack_pq = PathQuery::new_aggregate_sum_on_range(
            vec![TEST_LEAF.to_vec(), b"pct".to_vec()],
            QueryItem::RangeFrom(b"a".to_vec()..),
        );

        let result = GroveDb::verify_aggregate_sum_query(&forged_bytes, &attack_pq, v);
        assert!(
            result.is_err(),
            "ProvableCountTree at leaf must NOT be accepted for an aggregate-sum query"
        );
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("must be a ProvableSumTree"),
            "expected terminal-type error, got: {msg}"
        );
    }
}
