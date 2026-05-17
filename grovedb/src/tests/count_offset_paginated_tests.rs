//! End-to-end tests for offset-paginated proofs against
//! `ProvableCountTree` / `ProvableCountSumTree` merks.
//!
//! Lives at the GroveDB layer (not the merk layer) so the path-query
//! navigation + chain check is exercised — the merk-level unit tests
//! in `merk/src/proofs/query/count_offset/tests.rs` already cover the
//! pure prover/verifier roundtrip on a single merk.

#[cfg(test)]
mod tests {
    use grovedb_version::version::GroveVersion;

    use crate::{
        operations::proof::util::ProvedPathKeyValues, tests::make_test_grovedb, Element, GroveDb,
        PathQuery, Query, SizedQuery,
    };

    /// Build a fresh DB with `count_tree` (an empty `ProvableCountTree`)
    /// at the root, then insert keys "a" .. ('a' + n) into it, each
    /// mapped to a value of `format!("v_{}", key)`. Returns the DB and
    /// the keys as a `Vec<Vec<u8>>` in ascending order.
    fn make_provable_count_tree_with_n_items(
        n: u8,
        grove_version: &GroveVersion,
    ) -> (crate::tests::TempGroveDb, Vec<Vec<u8>>) {
        assert!(n <= 26, "fixture supports up to 26 single-letter keys");
        let db = make_test_grovedb(grove_version);
        db.insert(
            &[] as &[&[u8]],
            b"counts",
            Element::empty_provable_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert tree");
        let mut keys = Vec::with_capacity(n as usize);
        for i in 0..n {
            let key = vec![b'a' + i];
            let value = format!("v_{}", String::from_utf8_lossy(&key)).into_bytes();
            db.insert(
                &[b"counts"],
                key.as_slice(),
                Element::new_item(value),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert item");
            keys.push(key);
        }
        (db, keys)
    }

    /// Round-trip a single-range offset+limit query against a
    /// `ProvableCountTree`. Returns the verified items so callers can
    /// assert on key/value contents.
    fn round_trip_offset(
        db: &crate::tests::TempGroveDb,
        path: Vec<Vec<u8>>,
        query: Query,
        limit: Option<u16>,
        offset: Option<u16>,
        grove_version: &GroveVersion,
    ) -> ProvedPathKeyValues {
        let sized = SizedQuery::new(query, limit, offset);
        let path_query = PathQuery::new(path, sized);

        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove offset-paginated query");
        assert!(!proof.is_empty(), "proof bytes should be non-empty");

        let (root_hash, proved) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).expect("verify");
        let actual_root = db.root_hash(None, grove_version).unwrap().expect("root");
        assert_eq!(
            root_hash, actual_root,
            "verifier root hash should match the DB's actual root hash"
        );
        proved
    }

    fn proved_keys(proved: &ProvedPathKeyValues) -> Vec<Vec<u8>> {
        proved.iter().map(|p| p.key.clone()).collect()
    }

    #[test]
    fn end_to_end_offset_5_limit_3_ascending() {
        let v = GroveVersion::latest();
        let (db, _) = make_provable_count_tree_with_n_items(15, v);
        let mut q = Query::new();
        q.insert_range_inclusive(b"a".to_vec()..=b"o".to_vec());

        let proved = round_trip_offset(&db, vec![b"counts".to_vec()], q, Some(3), Some(5), v);
        assert_eq!(
            proved_keys(&proved),
            vec![b"f".to_vec(), b"g".to_vec(), b"h".to_vec()],
            "ascending: offset 5 + limit 3 should return f,g,h"
        );
    }

    #[test]
    fn end_to_end_offset_5_limit_3_descending() {
        let v = GroveVersion::latest();
        let (db, _) = make_provable_count_tree_with_n_items(15, v);
        let mut q = Query::new_with_direction(false); // right-to-left
        q.insert_range_inclusive(b"a".to_vec()..=b"o".to_vec());

        let proved = round_trip_offset(&db, vec![b"counts".to_vec()], q, Some(3), Some(5), v);
        assert_eq!(
            proved_keys(&proved),
            vec![b"j".to_vec(), b"i".to_vec(), b"h".to_vec()],
            "descending: offset 5 + limit 3 should return j,i,h"
        );
    }

    #[test]
    fn end_to_end_offset_past_end_returns_empty() {
        let v = GroveVersion::latest();
        let (db, _) = make_provable_count_tree_with_n_items(15, v);
        let mut q = Query::new();
        q.insert_range_inclusive(b"a".to_vec()..=b"o".to_vec());

        let proved = round_trip_offset(
            &db,
            vec![b"counts".to_vec()],
            q,
            Some(3),
            Some(100), // larger than the 15-item population
            v,
        );
        assert!(
            proved.is_empty(),
            "offset past the end yields zero returned items"
        );
    }

    #[test]
    fn end_to_end_offset_in_middle_of_partial_range() {
        let v = GroveVersion::latest();
        let (db, _) = make_provable_count_tree_with_n_items(15, v);
        // Restrict the range so some items are out-of-range, exercising
        // the Disjoint-subtree collapse alongside the offset machinery.
        let mut q = Query::new();
        q.insert_range_inclusive(b"c".to_vec()..=b"l".to_vec());

        let proved = round_trip_offset(&db, vec![b"counts".to_vec()], q, Some(3), Some(4), v);
        assert_eq!(
            proved_keys(&proved),
            vec![b"g".to_vec(), b"h".to_vec(), b"i".to_vec()],
            "ascending c..=l, offset 4 + limit 3 should return g,h,i"
        );
    }

    #[test]
    fn end_to_end_offset_with_limit_none_returns_remainder() {
        let v = GroveVersion::latest();
        let (db, _) = make_provable_count_tree_with_n_items(15, v);
        let mut q = Query::new();
        q.insert_range_inclusive(b"c".to_vec()..=b"l".to_vec());

        let proved = round_trip_offset(
            &db,
            vec![b"counts".to_vec()],
            q,
            None, // no limit → all remaining in-range
            Some(3),
            v,
        );
        assert_eq!(
            proved_keys(&proved),
            vec![
                b"f".to_vec(),
                b"g".to_vec(),
                b"h".to_vec(),
                b"i".to_vec(),
                b"j".to_vec(),
                b"k".to_vec(),
                b"l".to_vec(),
            ],
            "c..=l offset 3 with no limit returns f..l (7 items)"
        );
    }

    // ───────── SizedQuery::validate_count_offset_paginated unit tests ─────────
    //
    // Each branch in the validator gets its own test so a regression
    // (e.g. accidentally accepting a multi-item query) shows up as a
    // single failure with a clear message.

    use grovedb_merk::proofs::query::QueryItem;

    #[test]
    fn validate_rejects_no_offset() {
        // Calling the count-offset validator on a query that wasn't
        // even meant to be paginated is a programming error — surface
        // it as `InvalidQuery` instead of silently returning Ok.
        let mut q = Query::new();
        q.insert_all();
        let sized = SizedQuery::new(q, Some(5), None);
        let err = sized
            .validate_count_offset_paginated()
            .expect_err("no offset must reject");
        assert!(
            matches!(err, crate::Error::InvalidQuery(msg) if msg.contains("non-zero value")),
            "error should be InvalidQuery mentioning non-zero offset; got {:?}",
            err
        );
    }

    #[test]
    fn validate_rejects_offset_zero() {
        let mut q = Query::new();
        q.insert_all();
        let sized = SizedQuery::new(q, Some(5), Some(0));
        let err = sized
            .validate_count_offset_paginated()
            .expect_err("offset = 0 must reject");
        assert!(
            matches!(err, crate::Error::InvalidQuery(msg) if msg.contains("non-zero value")),
            "error should be InvalidQuery mentioning non-zero offset; got {:?}",
            err
        );
    }

    #[test]
    fn validate_rejects_aggregate_count_wrapper() {
        // AggregateCountOnRange has its own pagination semantics; we
        // reject it from this lane so the two flows don't shadow each
        // other.
        let mut q = Query::new();
        q.insert_item(QueryItem::AggregateCountOnRange(Box::new(
            QueryItem::RangeFull(std::ops::RangeFull),
        )));
        let sized = SizedQuery::new(q, Some(5), Some(2));
        let err = sized
            .validate_count_offset_paginated()
            .expect_err("aggregate count wrapper must reject");
        assert!(
            matches!(err, crate::Error::InvalidQuery(msg) if msg.contains("AggregateCountOnRange")),
            "error should be InvalidQuery mentioning AggregateCountOnRange; got {:?}",
            err
        );
    }

    #[test]
    fn validate_rejects_aggregate_sum_wrapper() {
        let mut q = Query::new();
        q.insert_item(QueryItem::AggregateSumOnRange(Box::new(
            QueryItem::RangeFull(std::ops::RangeFull),
        )));
        let sized = SizedQuery::new(q, Some(5), Some(2));
        let err = sized
            .validate_count_offset_paginated()
            .expect_err("aggregate sum wrapper must reject");
        assert!(
            matches!(err, crate::Error::InvalidQuery(msg) if msg.contains("AggregateSumOnRange")),
            "error should be InvalidQuery mentioning AggregateSumOnRange; got {:?}",
            err
        );
    }

    #[test]
    fn validate_rejects_default_subquery() {
        let mut q = Query::new();
        q.insert_all();
        q.default_subquery_branch.subquery = Some(Box::new(Query::new()));
        let sized = SizedQuery::new(q, Some(5), Some(2));
        let err = sized
            .validate_count_offset_paginated()
            .expect_err("default subquery must reject");
        assert!(
            matches!(err, crate::Error::InvalidQuery(msg) if msg.contains("default subquery branch")),
            "error should be InvalidQuery mentioning default subquery branch; got {:?}",
            err
        );
    }

    #[test]
    fn validate_rejects_default_subquery_path() {
        let mut q = Query::new();
        q.insert_all();
        q.default_subquery_branch.subquery_path = Some(vec![b"x".to_vec()]);
        let sized = SizedQuery::new(q, Some(5), Some(2));
        let err = sized
            .validate_count_offset_paginated()
            .expect_err("default subquery_path must reject");
        assert!(
            matches!(err, crate::Error::InvalidQuery(msg) if msg.contains("default subquery branch")),
            "error should be InvalidQuery mentioning default subquery branch; got {:?}",
            err
        );
    }

    #[test]
    fn validate_rejects_multi_item_query() {
        let mut q = Query::new();
        q.insert_key(b"a".to_vec());
        q.insert_key(b"b".to_vec());
        let sized = SizedQuery::new(q, Some(5), Some(2));
        let err = sized
            .validate_count_offset_paginated()
            .expect_err("multi-item query must reject");
        assert!(
            matches!(err, crate::Error::InvalidQuery(msg) if msg.contains("exactly one range QueryItem")),
            "error should be InvalidQuery mentioning single-item requirement; got {:?}",
            err
        );
    }

    #[test]
    fn validate_accepts_single_range_variants() {
        // Sanity: every ordinary range variant passes. `Key` is
        // deliberately excluded — see `validate_rejects_single_key`.
        let variants: Vec<QueryItem> = vec![
            QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
            QueryItem::RangeInclusive(b"a".to_vec()..=b"z".to_vec()),
            QueryItem::RangeFrom(b"a".to_vec()..),
            QueryItem::RangeFull(std::ops::RangeFull),
            QueryItem::RangeTo(..b"z".to_vec()),
            QueryItem::RangeToInclusive(..=b"z".to_vec()),
            QueryItem::RangeAfter(b"a".to_vec()..),
            QueryItem::RangeAfterTo(b"a".to_vec()..b"z".to_vec()),
            QueryItem::RangeAfterToInclusive(b"a".to_vec()..=b"z".to_vec()),
        ];
        for item in variants {
            let mut q = Query::new();
            q.insert_item(item.clone());
            let sized = SizedQuery::new(q, Some(5), Some(2));
            let result = sized.validate_count_offset_paginated();
            assert!(
                result.is_ok(),
                "variant {:?} should be accepted, got error {:?}",
                item,
                result.err()
            );
        }
    }

    #[test]
    fn validate_rejects_single_key() {
        // `QueryItem::Key` matches at most one in-range item, so
        // offset > 0 is structurally guaranteed to return zero items.
        // We reject this combination as a user error rather than
        // silently producing an empty result.
        let mut q = Query::new();
        q.insert_item(QueryItem::Key(b"a".to_vec()));
        let sized = SizedQuery::new(q, Some(5), Some(1));
        let err = sized
            .validate_count_offset_paginated()
            .expect_err("single-key + offset must reject");
        assert!(
            matches!(err, crate::Error::InvalidQuery(msg) if msg.contains("QueryItem::Key")),
            "error should be InvalidQuery mentioning the rejected variant; got {:?}",
            err
        );
    }

    #[test]
    fn path_query_validate_rejects_empty_path() {
        // PathQuery::validate_count_offset_paginated rejects empty
        // paths up-front: a count-offset query against the root
        // makes no sense because the root is always a NormalTree.
        let mut q = Query::new();
        q.insert_all();
        let pq = PathQuery::new(vec![], SizedQuery::new(q, Some(5), Some(2)));
        let err = pq
            .validate_count_offset_paginated()
            .expect_err("empty path must reject");
        assert!(
            matches!(err, crate::Error::InvalidQuery(msg) if msg.contains("root merk")),
            "error should be InvalidQuery mentioning root merk; got {:?}",
            err
        );
    }

    #[test]
    fn path_query_has_non_zero_offset() {
        let mut q = Query::new();
        q.insert_all();
        // offset = None → false
        let pq_none = PathQuery::new(vec![b"x".to_vec()], SizedQuery::new(q.clone(), None, None));
        assert!(!pq_none.has_non_zero_offset());
        // offset = Some(0) → false
        let pq_zero = PathQuery::new(
            vec![b"x".to_vec()],
            SizedQuery::new(q.clone(), None, Some(0)),
        );
        assert!(!pq_zero.has_non_zero_offset());
        // offset = Some(N) for N > 0 → true
        let pq_pos = PathQuery::new(vec![b"x".to_vec()], SizedQuery::new(q, None, Some(7)));
        assert!(pq_pos.has_non_zero_offset());
    }

    #[test]
    fn end_to_end_offset_rejects_with_subquery() {
        // Sanity: an offset query that fails the syntactic
        // `validate_count_offset_paginated` check must be rejected at
        // the prover entry, not silently fall through to the regular
        // proof path.
        let v = GroveVersion::latest();
        let (db, _) = make_provable_count_tree_with_n_items(5, v);

        let mut q = Query::new();
        q.insert_range_inclusive(b"a".to_vec()..=b"e".to_vec());
        // Add a default subquery branch — out-of-scope shape.
        q.default_subquery_branch.subquery = Some(Box::new(Query::new()));

        let path_query = PathQuery::new(
            vec![b"counts".to_vec()],
            SizedQuery::new(q, Some(3), Some(1)),
        );
        let result = db.prove_query(&path_query, None, v).unwrap();
        // The rejection MUST be `InvalidQuery` specifically — `is_err()`
        // alone would mask a regression where some unrelated error
        // (e.g. storage I/O) accidentally satisfies the test.
        assert!(
            matches!(result, Err(crate::Error::InvalidQuery(_))),
            "prover must reject offset on a query with a default subquery branch \
             with InvalidQuery; got {:?}",
            result
        );
    }

    #[test]
    fn end_to_end_offset_on_provable_count_sum_tree() {
        // `ProvableCountSumTree` shares the same `node_hash_with_count`
        // hashing rule as `ProvableCountTree` (the sum is stored on the
        // node but not bound to the hash), so the same `HashWithCount`
        // collapse op works for it. This test exercises that path
        // end-to-end through the grovedb layer.
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            &[] as &[&[u8]],
            b"counts_sum",
            Element::empty_provable_count_sum_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert provable count-sum tree");
        for i in 0..15u8 {
            let key = vec![b'a' + i];
            // `Element::new_item` stores plain Items, which contribute
            // 1 to count and 0 to sum (sum gates only fire for
            // sum-flavored values).
            db.insert(
                &[b"counts_sum"],
                key.as_slice(),
                Element::new_item(format!("v_{}", i).into_bytes()),
                None,
                None,
                v,
            )
            .unwrap()
            .expect("insert item");
        }

        let mut q = Query::new();
        q.insert_range_inclusive(b"a".to_vec()..=b"o".to_vec());
        let proved = round_trip_offset(&db, vec![b"counts_sum".to_vec()], q, Some(3), Some(5), v);
        assert_eq!(
            proved_keys(&proved),
            vec![b"f".to_vec(), b"g".to_vec(), b"h".to_vec()],
            "ProvableCountSumTree: offset 5 + limit 3 ascending should return f,g,h"
        );
    }

    #[test]
    fn end_to_end_offset_rejects_against_non_count_tree() {
        // Sanity: the syntactic gate accepts the query (single range,
        // no subqueries, offset > 0), but the leaf merk is a NormalTree
        // — the prover's leaf-level tree-type check should fire and
        // return InvalidQuery.
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            &[] as &[&[u8]],
            b"plain",
            Element::empty_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert tree");
        for i in 0..5u8 {
            let key = vec![b'a' + i];
            db.insert(
                &[b"plain"],
                key.as_slice(),
                Element::new_item(vec![i]),
                None,
                None,
                v,
            )
            .unwrap()
            .expect("insert");
        }

        let mut q = Query::new();
        q.insert_range_inclusive(b"a".to_vec()..=b"e".to_vec());
        let path_query = PathQuery::new(
            vec![b"plain".to_vec()],
            SizedQuery::new(q, Some(3), Some(1)),
        );
        let result = db.prove_query(&path_query, None, v).unwrap();
        // Same rationale as the subquery rejection test: pin the
        // exact error variant to detect regressions in the
        // tree-type gate's error normalization.
        assert!(
            matches!(result, Err(crate::Error::InvalidQuery(_))),
            "prover must reject offset against a NormalTree at leaf-open time \
             with InvalidQuery; got {:?}",
            result
        );
    }

    // ──────── V0 proof envelope coverage ────────
    //
    // Count-offset paginated proofs are V1-only. Grove versions v1 and
    // v2 (which use V0 proofs) reject any offset on a proved path query
    // — including count-offset paginated ones — unconditionally. The
    // tests below pin that V0 rejection contract; the V1 round-trips
    // above already exercise the positive path.

    use grovedb_version::version::v2::GROVE_V2;

    /// V0 proofs unconditionally reject `SizedQuery::offset` regardless
    /// of query shape. Pins the V0 prover entry's offset gate against
    /// accidental loosening (which would be a consensus-breaking change
    /// for grove v1/v2).
    #[test]
    fn v0_prover_rejects_offset_on_count_tree() {
        let v = &GROVE_V2;
        let (db, _) = make_provable_count_tree_with_n_items(5, v);
        let mut q = Query::new();
        q.insert_range_inclusive(b"a".to_vec()..=b"e".to_vec());
        let path_query = PathQuery::new(
            vec![b"counts".to_vec()],
            SizedQuery::new(q, Some(2), Some(1)),
        );
        let result = db.prove_query(&path_query, None, v).unwrap();
        assert!(
            matches!(result, Err(crate::Error::InvalidQuery(_))),
            "V0 prover must reject offsets unconditionally — V0 is a shipped wire \
             format and adding new accepted query shapes would be consensus-breaking. \
             Got {:?}",
            result
        );
    }

    /// V0 verifier counterpart: even if a caller hand-crafts a V0
    /// proof envelope and pairs it with an offset query, the verifier
    /// must reject. We can't easily forge a V0 proof here (the V0
    /// prover refuses to produce one), but we can pair an existing
    /// well-formed V0 proof (from a no-offset query) with a path-query
    /// that has offset set, and confirm the top-level entry rejects.
    #[test]
    fn v0_verifier_rejects_offset_on_query() {
        let v = &GROVE_V2;
        let (db, _) = make_provable_count_tree_with_n_items(5, v);

        // Produce a legitimate V0 proof for a no-offset query first.
        let mut q_no_offset = Query::new();
        q_no_offset.insert_range_inclusive(b"a".to_vec()..=b"e".to_vec());
        let pq_no_offset = PathQuery::new(
            vec![b"counts".to_vec()],
            SizedQuery::new(q_no_offset, Some(5), None),
        );
        let bytes = db
            .prove_query(&pq_no_offset, None, v)
            .unwrap()
            .expect("v0 prove for no-offset query");

        // Now pair those V0 bytes with an offset-bearing path query
        // and confirm the verifier refuses.
        let mut q_with_offset = Query::new();
        q_with_offset.insert_range_inclusive(b"a".to_vec()..=b"e".to_vec());
        let pq_with_offset = PathQuery::new(
            vec![b"counts".to_vec()],
            SizedQuery::new(q_with_offset, Some(2), Some(1)),
        );
        let result = GroveDb::verify_query_raw(&bytes, &pq_with_offset, v);
        assert!(
            matches!(result, Err(crate::Error::NotSupported(_))),
            "V0 verifier must reject offsets in path queries regardless of proof shape; \
             got {:?}",
            result
        );
    }

    /// Counterpart to `v0_verifier_rejects_offset_on_query` that goes
    /// through `verify_query` (with-options entry point), exercising
    /// `verify_proof_internal`'s offset gate rather than
    /// `verify_proof_raw_internal`'s. The two entry points share a
    /// helper (`apply_count_offset_envelope_gate`), but having a test
    /// behind each public surface ensures a refactor that accidentally
    /// drops the call on one side gets caught by CI.
    #[test]
    fn v0_verify_query_rejects_offset() {
        let v = &GROVE_V2;
        let (db, _) = make_provable_count_tree_with_n_items(5, v);

        let mut q_no_offset = Query::new();
        q_no_offset.insert_range_inclusive(b"a".to_vec()..=b"e".to_vec());
        let pq_no_offset = PathQuery::new(
            vec![b"counts".to_vec()],
            SizedQuery::new(q_no_offset, Some(5), None),
        );
        let bytes = db
            .prove_query(&pq_no_offset, None, v)
            .unwrap()
            .expect("v0 prove for no-offset query");

        let mut q_with_offset = Query::new();
        q_with_offset.insert_range_inclusive(b"a".to_vec()..=b"e".to_vec());
        let pq_with_offset = PathQuery::new(
            vec![b"counts".to_vec()],
            SizedQuery::new(q_with_offset, Some(2), Some(1)),
        );
        // `verify_query` → `verify_query_with_options` → `verify_proof_internal`.
        let result = GroveDb::verify_query(&bytes, &pq_with_offset, v);
        assert!(
            matches!(result, Err(crate::Error::NotSupported(_))),
            "verify_query (deserialized entry point) must reject offsets on V0 envelopes; \
             got {:?}",
            result
        );
    }

    /// Happy-path V1 round-trip going through `verify_query` (which
    /// dispatches via `verify_proof_internal` rather than the `_raw`
    /// variant). This exercises both the offset gate's V1 branch and
    /// the deserialized result path — keeping at least one happy-path
    /// case behind `verify_query` ensures the deserialized translation
    /// in `verify_proof_v1_internal` stays exercised even as
    /// `verify_query_raw` covers the canonical fast path.
    #[test]
    fn end_to_end_offset_via_verify_query() {
        let v = GroveVersion::latest();
        let (db, _) = make_provable_count_tree_with_n_items(15, v);
        let mut q = Query::new();
        q.insert_range_inclusive(b"a".to_vec()..=b"o".to_vec());
        let path_query = PathQuery::new(
            vec![b"counts".to_vec()],
            SizedQuery::new(q, Some(3), Some(5)),
        );

        let proof = db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove offset-paginated query");
        let (root_hash, deserialized) =
            GroveDb::verify_query(&proof, &path_query, v).expect("verify_query");
        assert_eq!(
            root_hash,
            db.root_hash(None, v).unwrap().expect("root"),
            "verify_query root hash should match the DB's actual root hash",
        );
        let returned_keys: Vec<Vec<u8>> =
            deserialized.iter().map(|(_, key, _)| key.clone()).collect();
        assert_eq!(
            returned_keys,
            vec![b"f".to_vec(), b"g".to_vec(), b"h".to_vec()],
            "verify_query happy path: offset 5 + limit 3 over a..=o should return f,g,h",
        );
    }

    // ──────── lower_layers / non-empty-tree return rejections ────────

    /// Soundness regression test for the
    /// `layer_proof.lower_layers.is_empty()` check (CodeRabbit
    /// review on grovedb#669). An honest count-offset prover always
    /// emits empty `lower_layers` (the validator rejects subqueries),
    /// so we forge a proof envelope with a stray child layer attached
    /// and confirm the verifier rejects.
    ///
    /// The forging is done by decoding a legitimate proof envelope,
    /// injecting a `lower_layers` entry, re-encoding, and feeding the
    /// result to `verify_query_raw`.
    #[test]
    fn rejects_count_offset_proof_with_forged_lower_layers() {
        use crate::operations::proof::{GroveDBProof, GroveDBProofV1, LayerProof, ProofBytes};

        let v = GroveVersion::latest();
        let (db, _) = make_provable_count_tree_with_n_items(15, v);
        let mut q = Query::new();
        q.insert_range_inclusive(b"a".to_vec()..=b"o".to_vec());
        let path_query = PathQuery::new(
            vec![b"counts".to_vec()],
            SizedQuery::new(q, Some(3), Some(5)),
        );

        // Generate an honest proof, then surgically corrupt the
        // leaf-layer's lower_layers map.
        let honest_proof = db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove");

        // Decode the envelope so we can mutate it.
        let bincode_config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        let (decoded, _) =
            bincode::decode_from_slice::<GroveDBProof, _>(honest_proof.as_slice(), bincode_config)
                .expect("decode envelope");
        let GroveDBProof::V1(GroveDBProofV1 { mut root_layer }) = decoded else {
            panic!("expected V1 proof");
        };

        // Locate the leaf (count_tree) layer at "counts" and attach a
        // bogus child entry that an honest prover would never emit.
        let leaf = root_layer
            .lower_layers
            .get_mut(b"counts".as_slice())
            .expect("leaf layer present");
        leaf.lower_layers.insert(
            b"forged_child".to_vec(),
            LayerProof {
                merk_proof: ProofBytes::Merk(vec![]),
                lower_layers: Default::default(),
            },
        );

        let tampered = bincode::encode_to_vec(
            GroveDBProof::V1(GroveDBProofV1 { root_layer }),
            bincode_config,
        )
        .expect("encode tampered");

        let result = GroveDb::verify_query_raw(&tampered, &path_query, v);
        assert!(
            matches!(result, Err(crate::Error::InvalidProof(_, _))),
            "verifier must reject forged lower_layers in count-offset leaf; got {:?}",
            result
        );
    }

    /// Soundness regression test for the non-empty-tree return
    /// rejection (CodeRabbit review on grovedb#669). The current
    /// count-offset prover doesn't emit
    /// `KVValueHashFeatureTypeWithChildHash`, so a non-empty tree
    /// returned via this path would silently bypass the V1 strict-mode
    /// child-hash invariant. The verifier explicitly rejects such
    /// returns with `Error::NotSupported`.
    #[test]
    fn rejects_count_offset_with_non_empty_tree_return() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            &[] as &[&[u8]],
            b"counts",
            Element::empty_provable_count_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert count tree");
        // Tree fixture: "a" = Item, "b" = non-empty Tree, "c" = Item.
        // With offset=1, limit=1 (ascending), the verifier walks
        // past "a" (offset) and the next returned item is the
        // non-empty tree "b" — exactly the case we want to reject.
        db.insert(
            &[b"counts"],
            b"a",
            Element::new_item(b"v_a".to_vec()),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert a");
        db.insert(&[b"counts"], b"b", Element::empty_tree(), None, None, v)
            .unwrap()
            .expect("insert inner tree b");
        // Populate the inner tree so it becomes non-empty.
        db.insert(
            [b"counts".as_slice(), b"b".as_slice()].as_slice(),
            b"inner",
            Element::new_item(b"x".to_vec()),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("populate inner tree");
        db.insert(
            &[b"counts"],
            b"c",
            Element::new_item(b"v_c".to_vec()),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert c");

        let mut q = Query::new();
        q.insert_range_inclusive(b"a".to_vec()..=b"z".to_vec());
        let path_query = PathQuery::new(
            vec![b"counts".to_vec()],
            // offset=1 skips "a", limit=1 returns the next item ("b",
            // the non-empty tree).
            SizedQuery::new(q, Some(1), Some(1)),
        );
        // The prover now rejects this case up-front via the merk-level
        // descent check — it refuses to produce an honest proof that
        // the verifier would later reject. We assert the prover-side
        // rejection specifically.
        let result = db.prove_query(&path_query, None, v).unwrap();
        let err = result.expect_err("prover must reject non-empty tree return");
        let msg = format!("{}", err);
        assert!(
            msg.contains("non-empty tree"),
            "prover rejection should mention the non-empty tree limitation; got {}",
            msg
        );
    }

    /// Prover-side rejection for `NonCounted`-wrapped in-range entries.
    /// Earlier drafts silently dropped these from the result (own_count
    /// = 0 → no return); the merk prover now refuses to descend through
    /// them, surfacing the divergence from regular-GroveDB semantics
    /// explicitly.
    #[test]
    fn rejects_count_offset_with_non_counted_entry() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            &[] as &[&[u8]],
            b"counts",
            Element::empty_provable_count_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert count tree");

        // Fixture: a = counted Item, b = NonCounted(Item), c = counted Item.
        // With offset=1, the prover descends through b (skipping
        // counted items in the tree's count-aware order). Hitting b
        // must surface as an error rather than silently producing a
        // proof that drops it.
        db.insert(
            &[b"counts"],
            b"a",
            Element::new_item(b"v_a".to_vec()),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert a");
        db.insert(
            &[b"counts"],
            b"b",
            Element::new_non_counted(Element::new_item(b"v_b".to_vec())).expect("non_counted wrap"),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert NonCounted(b)");
        db.insert(
            &[b"counts"],
            b"c",
            Element::new_item(b"v_c".to_vec()),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert c");

        let mut q = Query::new();
        q.insert_range_inclusive(b"a".to_vec()..=b"z".to_vec());
        let path_query = PathQuery::new(
            vec![b"counts".to_vec()],
            SizedQuery::new(q, Some(2), Some(1)),
        );
        let result = db.prove_query(&path_query, None, v).unwrap();
        let err = result.expect_err("prover must reject NonCounted in-range entry");
        let msg = format!("{}", err);
        assert!(
            msg.contains("NonCounted"),
            "prover rejection should mention NonCounted; got {}",
            msg
        );
    }

    /// Prover-side rejection for `Reference` in-range entries. Earlier
    /// drafts returned the raw `Element::Reference` bytes verbatim
    /// because the count-offset short-circuit doesn't run the regular
    /// flow's reference post-pass. The prover now refuses to emit
    /// these.
    #[test]
    fn rejects_count_offset_with_reference_entry() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            &[] as &[&[u8]],
            b"counts",
            Element::empty_provable_count_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert count tree");
        // The reference target.
        db.insert(
            &[b"counts"],
            b"a",
            Element::new_item(b"target_value".to_vec()),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert a");
        // The reference pointing at "a".
        use crate::reference_path::ReferencePathType;
        db.insert(
            &[b"counts"],
            b"b",
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                b"counts".to_vec(),
                b"a".to_vec(),
            ])),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert reference b");
        db.insert(
            &[b"counts"],
            b"c",
            Element::new_item(b"v_c".to_vec()),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert c");

        let mut q = Query::new();
        q.insert_range_inclusive(b"a".to_vec()..=b"z".to_vec());
        let path_query = PathQuery::new(
            vec![b"counts".to_vec()],
            SizedQuery::new(q, Some(2), Some(1)),
        );
        let result = db.prove_query(&path_query, None, v).unwrap();
        let err = result.expect_err("prover must reject Reference in-range entry");
        let msg = format!("{}", err);
        assert!(
            msg.contains("Reference"),
            "prover rejection should mention Reference; got {}",
            msg
        );
    }

    // ──────── check_count_offset_target_tree_type error normalization ────────
    //
    // Targets the `Err(_e)` branch of the helper in `generate.rs` —
    // when the target path does not resolve to an openable merk at
    // all, we still want a clean `InvalidQuery` instead of leaking
    // a storage-layer error to the caller.

    #[test]
    fn end_to_end_offset_rejects_against_nonexistent_path() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        // Don't insert anything at "missing" — opening it will fail.
        let mut q = Query::new();
        q.insert_range_inclusive(b"a".to_vec()..=b"z".to_vec());
        let path_query = PathQuery::new(
            vec![b"missing".to_vec()],
            SizedQuery::new(q, Some(3), Some(1)),
        );
        let result = db.prove_query(&path_query, None, v).unwrap();
        // The `open_transactional_merk_at_path` failure inside
        // `check_count_offset_target_tree_type` is normalized to
        // `InvalidQuery` — not surfaced as a raw storage error.
        assert!(
            matches!(result, Err(crate::Error::InvalidQuery(_))),
            "prover must reject offset against a nonexistent path with \
             InvalidQuery; got {:?}",
            result
        );
    }

    // ──────── Forged-proof tests for verifier defense-in-depth ────────
    //
    // The merk-level prover now refuses to emit NonCounted-wrapped /
    // Reference / non-empty-tree in-range entries (see the three
    // `rejects_count_offset_with_*` tests above). That makes the
    // GroveDB-layer defense-in-depth checks in
    // `run_count_offset_layer_dispatch` (verify.rs ~537-566) unreachable
    // by **honest** proofs. To keep those branches exercised — they're
    // the only guard against a forged proof that bypassed the prover —
    // these tests build a legitimate proof, surgically rewrite one
    // value-bearing proof node in the leaf merk to carry forged value
    // bytes, and confirm each defense-in-depth branch rejects the
    // expected element shape.
    //
    // Forge mechanism: replace `KVCount(key, value, count)` (what the
    // prover emits for ProvableCountedMerkNode Items) with
    // `KVValueHashFeatureType(key, FORGED_VALUE, H(original_value),
    // ProvableCountedMerkNode(count))`. The merk-level kv_hash is
    // computed from the committed value_hash field, not from the
    // value bytes — so the merk-level chain hash stays intact, the
    // count check (`provable_count_from_aggregate`) still returns the
    // right count, and the count-offset verifier surfaces the forged
    // value bytes into `CountOffsetReturnedItem.value`. The
    // GroveDB-layer `Element::deserialize` then triggers the right
    // defense-in-depth rejection.

    /// Helper for the forge: take an honest proof, find the
    /// `KVCount(key, value, count)` op for `target_key` in the leaf
    /// merk_proof under `b"counts"`, replace it with a forged
    /// `KVValueHashFeatureType` carrying `forged_value` (and the
    /// original value_hash so the merk chain still verifies), re-encode
    /// the proof, and return the tampered envelope bytes.
    fn forge_count_offset_proof_replacing_value(
        honest_proof: Vec<u8>,
        target_key: &[u8],
        forged_value: Vec<u8>,
    ) -> Vec<u8> {
        use std::collections::LinkedList;

        use bincode::{decode_from_slice, encode_to_vec};
        use grovedb_merk::{
            proofs::{encode_into, Decoder, Node, Op},
            tree::{kv_digest_to_kv_hash as _, value_hash, TreeFeatureType},
        };

        use crate::operations::proof::{GroveDBProof, GroveDBProofV1, ProofBytes};

        let cfg = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        let (decoded, _) =
            decode_from_slice::<GroveDBProof, _>(honest_proof.as_slice(), cfg).expect("decode");
        let GroveDBProof::V1(GroveDBProofV1 { mut root_layer }) = decoded else {
            panic!("expected V1 proof");
        };

        // The leaf merk_proof lives under "counts".
        let leaf = root_layer
            .lower_layers
            .get_mut(b"counts".as_slice())
            .expect("leaf layer at counts");
        let original_bytes = match &leaf.merk_proof {
            ProofBytes::Merk(b) => b.clone(),
            _ => panic!("leaf merk_proof must be ProofBytes::Merk"),
        };

        // Walk the ops; replace the first matching KVCount op.
        let mut ops: LinkedList<Op> = LinkedList::new();
        let decoder = Decoder::new(&original_bytes);
        let mut replaced = false;
        for op in decoder {
            let op = op.expect("decode proof op");
            let new_op = match op {
                Op::Push(Node::KVCount(ref key, ref value, count)) if key == target_key => {
                    let vh = value_hash(value).unwrap();
                    replaced = true;
                    Op::Push(Node::KVValueHashFeatureType(
                        key.clone(),
                        forged_value.clone(),
                        vh,
                        TreeFeatureType::ProvableCountedMerkNode(count),
                    ))
                }
                other => other,
            };
            ops.push_back(new_op);
        }
        assert!(
            replaced,
            "forge: target_key {:?} not found as KVCount in the honest proof — \
             test fixture / proof layout has diverged",
            target_key
        );

        let mut new_bytes = Vec::with_capacity(original_bytes.len() + forged_value.len());
        encode_into(ops.iter(), &mut new_bytes);
        leaf.merk_proof = ProofBytes::Merk(new_bytes);

        encode_to_vec(GroveDBProof::V1(GroveDBProofV1 { root_layer }), cfg)
            .expect("encode tampered envelope")
    }

    /// Builds the standard 15-item ProvableCountTree fixture, generates
    /// an honest offset-paginated proof returning {"f", "g", "h"}, then
    /// returns the proof and the path-query so individual forge tests
    /// can target one of the in-range keys.
    fn forge_fixture() -> (crate::tests::TempGroveDb, Vec<u8>, PathQuery) {
        let v = GroveVersion::latest();
        let (db, _) = make_provable_count_tree_with_n_items(15, v);
        let mut q = Query::new();
        q.insert_range_inclusive(b"a".to_vec()..=b"o".to_vec());
        let path_query = PathQuery::new(
            vec![b"counts".to_vec()],
            SizedQuery::new(q, Some(3), Some(5)),
        );
        let honest = db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove honest");
        (db, honest, path_query)
    }

    /// Defense-in-depth: a forged proof that surfaces a NonCounted
    /// element in `returned_items` must be rejected as `InvalidProof`
    /// mentioning "NonCounted" — the merk prover refuses to emit these,
    /// so reaching the GroveDB-layer check means the proof was forged.
    #[test]
    fn verifier_rejects_forged_non_counted_returned_item() {
        let v = GroveVersion::latest();
        let (_db, honest, path_query) = forge_fixture();
        let forged_elem =
            Element::new_non_counted(Element::new_item(b"forged_item".to_vec())).expect("wrap nc");
        let forged_bytes = forged_elem.serialize(v).expect("serialize forged");
        let tampered = forge_count_offset_proof_replacing_value(honest, b"f", forged_bytes);
        let result = GroveDb::verify_query_raw(&tampered, &path_query, v);
        let err = result.expect_err("forged NonCounted return must be rejected");
        assert!(
            matches!(err, crate::Error::InvalidProof(_, ref msg) if msg.contains("NonCounted")),
            "forged NonCounted return should reject as InvalidProof mentioning NonCounted; got {:?}",
            err,
        );
    }

    /// Defense-in-depth: a forged proof that surfaces a Reference
    /// element in `returned_items` must be rejected as `NotSupported`
    /// mentioning "Reference" — the count-offset short-circuit doesn't
    /// run the regular flow's reference post-pass, so accepting one
    /// would surface a raw `Element::Reference` to the caller.
    #[test]
    fn verifier_rejects_forged_reference_returned_item() {
        use crate::reference_path::ReferencePathType;
        let v = GroveVersion::latest();
        let (_db, honest, path_query) = forge_fixture();
        let forged_elem = Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            b"counts".to_vec(),
            b"a".to_vec(),
        ]));
        let forged_bytes = forged_elem.serialize(v).expect("serialize forged");
        let tampered = forge_count_offset_proof_replacing_value(honest, b"f", forged_bytes);
        let result = GroveDb::verify_query_raw(&tampered, &path_query, v);
        let err = result.expect_err("forged Reference return must be rejected");
        assert!(
            matches!(err, crate::Error::NotSupported(ref msg) if msg.contains("Reference")),
            "forged Reference return should reject as NotSupported mentioning Reference; got {:?}",
            err,
        );
    }

    /// Defense-in-depth: a forged proof that surfaces a non-empty Tree
    /// (i.e. an inner subtree with a `Some(root_key)`) must be rejected
    /// as `NotSupported` — V1 strict-mode would require a
    /// `KVValueHashFeatureTypeWithChildHash` proof node, which the
    /// current count-offset prover never emits.
    #[test]
    fn verifier_rejects_forged_non_empty_tree_returned_item() {
        let v = GroveVersion::latest();
        let (_db, honest, path_query) = forge_fixture();
        // A bare `Element::Tree(Some(root_key), flags)` has
        // `is_non_empty_tree() == true`. The root key bytes are
        // arbitrary — the defense-in-depth check fires on the type
        // shape alone.
        let forged_elem = Element::Tree(Some(vec![0xAB; 32]), None);
        let forged_bytes = forged_elem.serialize(v).expect("serialize forged");
        let tampered = forge_count_offset_proof_replacing_value(honest, b"f", forged_bytes);
        let result = GroveDb::verify_query_raw(&tampered, &path_query, v);
        let err = result.expect_err("forged non-empty tree return must be rejected");
        assert!(
            matches!(err, crate::Error::NotSupported(ref msg) if msg.contains("non-empty tree")),
            "forged non-empty tree return should reject as NotSupported mentioning \
             non-empty tree; got {:?}",
            err,
        );
    }
}
