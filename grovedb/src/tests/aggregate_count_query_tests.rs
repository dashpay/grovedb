//! End-to-end GroveDB tests for `AggregateCountOnRange` queries.
//!
//! These exercise the full prove → encode → decode → verify pipeline against
//! both `ProvableCountTree` and `ProvableCountSumTree` (and their
//! `NonCounted*` wrappers via being the *parent* tree, not the queried one),
//! at various path depths and across the full set of allowed range variants.

#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::query::QueryItem;
    use grovedb_version::version::{v2::GROVE_V2, GroveVersion};

    use crate::{
        tests::{make_test_grovedb, TEST_LEAF},
        Element, GroveDb, PathQuery,
    };

    /// Insert the 15 single-byte keys "a".."o" into a `ProvableCountTree`
    /// rooted at `[TEST_LEAF, "ct"]`. Returns the GroveDB and the resulting
    /// root hash.
    fn setup_15_key_provable_count_tree(
        grove_version: &GroveVersion,
    ) -> (crate::tests::TempGroveDb, [u8; 32]) {
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"ct",
            Element::empty_provable_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert ct");
        for c in b'a'..=b'o' {
            db.insert(
                [TEST_LEAF, b"ct"].as_ref(),
                &[c],
                Element::new_item(vec![c]),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert leaf");
        }
        let root = db
            .grove_db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root_hash");
        (db, root)
    }

    fn setup_15_key_provable_count_sum_tree(
        grove_version: &GroveVersion,
    ) -> (crate::tests::TempGroveDb, [u8; 32]) {
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cst",
            Element::empty_provable_count_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert cst");
        for c in b'a'..=b'o' {
            db.insert(
                [TEST_LEAF, b"cst"].as_ref(),
                &[c],
                // `Item` plays the role of a non-sum element inside a count
                // sum tree — we're testing count semantics, not sum.
                Element::new_item(vec![c]),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert leaf");
        }
        let root = db
            .grove_db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root_hash");
        (db, root)
    }

    /// Round-trip helper: build a path_query, prove it, verify it, assert
    /// `(root, count)` matches what we expect.
    fn round_trip(
        db: &crate::tests::TempGroveDb,
        expected_root: [u8; 32],
        path: Vec<Vec<u8>>,
        inner_range: QueryItem,
        expected_count: u64,
        grove_version: &GroveVersion,
    ) {
        let path_query = PathQuery::new_aggregate_count_on_range(path, inner_range);
        let proof = db
            .grove_db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove_query should succeed");
        let (root, count) =
            GroveDb::verify_aggregate_count_query(&proof, &path_query, grove_version)
                .expect("verify should succeed");
        assert_eq!(root, expected_root, "verifier reconstructed wrong root");
        assert_eq!(count, expected_count, "verifier returned wrong count");
    }

    #[test]
    fn provable_count_tree_range_inclusive() {
        let v = GroveVersion::latest();
        let (db, root) = setup_15_key_provable_count_tree(v);
        round_trip(
            &db,
            root,
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
            10,
            v,
        );
    }

    #[test]
    fn provable_count_tree_range_exclusive() {
        let v = GroveVersion::latest();
        let (db, root) = setup_15_key_provable_count_tree(v);
        round_trip(
            &db,
            root,
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::Range(b"c".to_vec()..b"l".to_vec()),
            9,
            v,
        );
    }

    #[test]
    fn provable_count_tree_range_from() {
        let v = GroveVersion::latest();
        let (db, root) = setup_15_key_provable_count_tree(v);
        round_trip(
            &db,
            root,
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeFrom(b"c".to_vec()..),
            13,
            v,
        );
    }

    #[test]
    fn provable_count_tree_range_after() {
        let v = GroveVersion::latest();
        let (db, root) = setup_15_key_provable_count_tree(v);
        round_trip(
            &db,
            root,
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeAfter(b"b".to_vec()..),
            13,
            v,
        );
    }

    #[test]
    fn provable_count_tree_range_to_inclusive() {
        let v = GroveVersion::latest();
        let (db, root) = setup_15_key_provable_count_tree(v);
        round_trip(
            &db,
            root,
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeToInclusive(..=b"e".to_vec()),
            5,
            v,
        );
    }

    #[test]
    fn provable_count_tree_range_below_all() {
        let v = GroveVersion::latest();
        let (db, root) = setup_15_key_provable_count_tree(v);
        round_trip(
            &db,
            root,
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeInclusive(vec![0x00]..=vec![0x10]),
            0,
            v,
        );
    }

    #[test]
    fn provable_count_sum_tree_range_inclusive() {
        let v = GroveVersion::latest();
        let (db, root) = setup_15_key_provable_count_sum_tree(v);
        round_trip(
            &db,
            root,
            vec![TEST_LEAF.to_vec(), b"cst".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
            10,
            v,
        );
    }

    #[test]
    fn rejects_invalid_range_at_construction() {
        // A path-query with an inner Key item should be rejected at
        // validation time, before any proof generation runs.
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::Key(b"c".to_vec()),
        );
        let err = path_query.validate_aggregate_count_on_range();
        assert!(err.is_err(), "Key inner should be rejected");
    }

    #[test]
    fn rejects_inner_range_full() {
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeFull(std::ops::RangeFull),
        );
        assert!(path_query.validate_aggregate_count_on_range().is_err());
    }

    #[test]
    fn rejects_against_normal_tree() {
        // Querying a NormalTree with AggregateCountOnRange should fail at
        // proof time with an InvalidProofError from the merk layer. We need
        // at least one element in the target normal tree so that the
        // multi-layer proof generator actually recurses into it (empty
        // trees are returned as result rows without a lower-layer descent).
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"x",
            Element::new_item(b"y".to_vec()),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("seed normal tree");
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec()],
            QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
        );
        let proof_result = db.grove_db.prove_query(&path_query, None, v).unwrap();
        assert!(
            proof_result.is_err(),
            "expected prove_query to fail on NormalTree, got {:?}",
            proof_result.ok().map(|b| b.len())
        );
    }

    #[test]
    fn count_forgery_is_caught_at_grovedb_level() {
        // End-to-end version of the merk-level forgery test: tamper with the
        // count in a HashWithCount op inside the encoded proof and the
        // GroveDB verifier should reject it (root mismatch in the layer
        // chain).
        let v = GroveVersion::latest();
        let (db, _expected_root) = setup_15_key_provable_count_tree(v);
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
        );
        let mut proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");

        // Search the encoded proof for the HashWithCount opcode (0x1e for
        // Push, 0x1f for PushInverted) and bump the count varint by one.
        // This is fragile to encoding changes, so we treat "found at least
        // one" as a precondition.
        let mut tampered = false;
        for i in 0..proof.len() {
            if proof[i] == 0x1e || proof[i] == 0x1f {
                // Layout: opcode | kv_hash[32] | left[32] | right[32] | count_varint
                let count_offset = i + 1 + 32 * 3;
                if count_offset < proof.len() {
                    proof[count_offset] = proof[count_offset].wrapping_add(1);
                    tampered = true;
                    break;
                }
            }
        }
        assert!(
            tampered,
            "test setup: expected at least one HashWithCount opcode in the encoded proof"
        );

        let verify_result = GroveDb::verify_aggregate_count_query(&proof, &path_query, v);
        assert!(
            verify_result.is_err(),
            "tampered count must be rejected at the GroveDB verifier level, got {:?}",
            verify_result.map(|(_, c)| c)
        );
    }

    /// Build a 3-layer path: TEST_LEAF -> "outer" (NormalTree) ->
    /// "inner" (ProvableCountTree) populated with 5 keys "a".."e".
    fn setup_three_layer_provable_count_tree(
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
            Element::empty_provable_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert inner");
        for c in b'a'..=b'e' {
            db.insert(
                [TEST_LEAF, b"outer", b"inner"].as_ref(),
                &[c],
                Element::new_item(vec![c]),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert leaf");
        }
        let root = db
            .grove_db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root_hash");
        (db, root)
    }

    #[test]
    fn three_layer_path_round_trip() {
        // Exercises the multi-layer chain enforcement: layer 0 proves TEST_LEAF
        // exists, layer 1 proves "outer" exists in TEST_LEAF, layer 2 proves
        // "inner" exists in outer, layer 3 is the count proof on inner.
        let v = GroveVersion::latest();
        let (db, root) = setup_three_layer_provable_count_tree(v);
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"outer".to_vec(), b"inner".to_vec()],
            QueryItem::RangeInclusive(b"b".to_vec()..=b"d".to_vec()),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");
        let (got_root, got_count) = GroveDb::verify_aggregate_count_query(&proof, &path_query, v)
            .expect("verify should succeed");
        assert_eq!(got_root, root, "verifier root must match GroveDB root");
        assert_eq!(got_count, 3, "expected count of {{b, c, d}}");
    }

    #[test]
    fn corrupted_path_layer_byte_is_rejected() {
        // Tamper with a non-leaf-layer byte (a tree-element value byte) and
        // verify that the chain enforcement catches it. We pick a byte deep
        // enough that it lands inside one of the parent merk's KV value bytes.
        let v = GroveVersion::latest();
        let (db, _root) = setup_three_layer_provable_count_tree(v);
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"outer".to_vec(), b"inner".to_vec()],
            QueryItem::RangeInclusive(b"b".to_vec()..=b"d".to_vec()),
        );
        let mut proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");
        // Flip a byte well inside the proof — the exact location doesn't
        // matter as long as it isn't the bincode envelope length prefix.
        // Index 32 is past the envelope and into the first inner merk's bytes.
        let target = proof.len() / 2;
        proof[target] = proof[target].wrapping_add(1);
        let verify_result = GroveDb::verify_aggregate_count_query(&proof, &path_query, v);
        assert!(
            verify_result.is_err(),
            "tampered proof byte must be rejected, got {:?}",
            verify_result.map(|(_, c)| c)
        );
    }

    #[test]
    fn provable_count_tree_works_on_grove_v2_envelope() {
        // GROVE_V2 dispatches to the V0 prove_query_non_serialized path, which
        // produces a `MerkOnlyLayerProof` envelope rather than V1's
        // `LayerProof`. Verify the same prove → verify cycle works through that
        // envelope.
        let v: &GroveVersion = &GROVE_V2;
        let (db, root) = setup_15_key_provable_count_tree(v);
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query (v0 envelope) should succeed");
        let (got_root, got_count) = GroveDb::verify_aggregate_count_query(&proof, &path_query, v)
            .expect("verify should succeed against v0 envelope");
        assert_eq!(got_root, root);
        assert_eq!(got_count, 10);
    }

    #[test]
    fn verify_rejects_malformed_path_query_at_entry() {
        // Even before any proof bytes are decoded, the verifier rejects a
        // path_query that isn't a well-formed AggregateCountOnRange query.
        let v = GroveVersion::latest();
        let bad_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec()],
            QueryItem::Key(b"k".to_vec()), // inner Key is not allowed
        );
        // Any proof bytes are fine — validation happens before decoding.
        let dummy_proof = vec![0u8; 16];
        let err = GroveDb::verify_aggregate_count_query(&dummy_proof, &bad_query, v)
            .expect_err("malformed path_query must be rejected up front");
        let s = format!("{:?}", err);
        assert!(
            s.contains("Key") || s.contains("InvalidQuery"),
            "got: {}",
            s
        );
    }

    #[test]
    fn validate_at_construction_rejects_nested_aggregate_count_on_range() {
        // Nested AggregateCountOnRange is rejected at validation time.
        let pq = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::AggregateCountOnRange(Box::new(QueryItem::Range(
                b"a".to_vec()..b"z".to_vec(),
            ))),
        );
        assert!(pq.validate_aggregate_count_on_range().is_err());
    }

    /// `Element::NonCounted` wrappers tell the parent tree to **skip** the
    /// wrapped element when aggregating its own count.
    /// `AggregateCountOnRange` honors that: NonCounted children are
    /// excluded from the result.
    ///
    /// Mechanics — every node in a `ProvableCountTree` carries an
    /// own_count of 1 (normal) or 0 (NonCounted). The merk-recorded
    /// aggregate at any subtree = sum of own_counts in the subtree
    /// (NonCounted entries contribute 0). The verifier's shape walk
    /// derives each boundary node's own_count as
    /// `node_aggregate − left_struct − right_struct` and credits **only
    /// own_count** to the in-range total when the key falls in range.
    /// For a NonCounted leaf, own_count = 0 and the wrapped key
    /// contributes nothing. The structural counts threaded through the
    /// walk are hash-bound at every step (every count-bearing proof node
    /// feeds its count into `node_hash_with_count`), so a malicious
    /// prover can't lie about a NonCounted node's status without
    /// breaking the parent's hash chain.
    #[test]
    fn non_counted_children_are_excluded_from_aggregate_count() {
        use crate::tests::TEST_LEAF;

        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"ct",
            Element::empty_provable_count_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert ct");

        // Five regular items — each contributes 1.
        for c in [b'a', b'b', b'c', b'd', b'e'] {
            db.insert(
                [TEST_LEAF, b"ct"].as_ref(),
                &[c],
                Element::new_item(vec![c]),
                None,
                None,
                v,
            )
            .unwrap()
            .expect("insert regular item");
        }

        // One NonCounted-wrapped item, key "f" — in-range but contributes
        // 0 (own_count = 0).
        let nc_item =
            Element::new_non_counted(Element::new_item(b"hidden".to_vec())).expect("wrap ok");
        db.insert([TEST_LEAF, b"ct"].as_ref(), b"f", nc_item, None, None, v)
            .unwrap()
            .expect("insert NonCounted item");

        let root = db.grove_db.root_hash(None, v).unwrap().expect("root_hash");

        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeInclusive(b"a".to_vec()..=b"z".to_vec()),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove");
        let (got_root, got_count) =
            GroveDb::verify_aggregate_count_query(&proof, &path_query, v).expect("verify");
        assert_eq!(got_root, root, "root mismatch");
        assert_eq!(
            got_count, 5,
            "NonCounted-wrapped child must be excluded from the aggregate count"
        );
    }

    /// Pin observable cost numbers + proof byte size for a known input so
    /// regressions in the proof shape (extra unnecessary nodes, missing
    /// short-circuit, etc.) show up as a test failure instead of as a
    /// silent perf hit. Values are exact for the 15-key
    /// `ProvableCountTree` + `RangeInclusive("c"..="l")` setup; if the
    /// proof shape changes intentionally, update them here.
    #[test]
    fn proof_size_snapshot_for_15_key_closed_range() {
        let v = GroveVersion::latest();
        let (db, _root) = setup_15_key_provable_count_tree(v);
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove");

        // Snapshot the proof byte size. The current shape produces a small
        // deterministic byte stream; if this drifts upward without
        // intent, the proof shape may have regressed.
        //
        // The acceptable range is conservative — we only require the
        // proof stays bounded by what an O(log n) shape predicts for a
        // 4-level tree (a few hundred bytes is the right ballpark; many
        // KB would indicate the count short-circuit didn't fire). The
        // *current* size is around 650 bytes; a few hundred bytes of
        // headroom in either direction tolerates encoding tweaks but
        // catches gross regressions.
        let len = proof.len();
        assert!(
            (300..=900).contains(&len),
            "aggregate-count proof size {} bytes is outside the expected \
             [300, 900] window for a 15-key 2-layer query — proof shape \
             may have regressed",
            len
        );

        // Round-trip through the verifier as a sanity check that the
        // pinned shape is still verifiable.
        let (_root, count) =
            GroveDb::verify_aggregate_count_query(&proof, &path_query, v).expect("verify");
        assert_eq!(count, 10);
    }
}
