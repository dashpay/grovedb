//! End-to-end GroveDB tests for `AggregateCountOnRange` queries.
//!
//! These exercise the full prove → encode → decode → verify pipeline against
//! both `ProvableCountTree` and `ProvableCountSumTree` (and their
//! `NonCounted*` wrappers via being the *parent* tree, not the queried one),
//! at various path depths and across the full set of allowed range variants.

#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::query::QueryItem;
    use grovedb_version::version::GroveVersion;

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
}
