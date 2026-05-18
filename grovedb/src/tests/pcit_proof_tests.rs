//! End-to-end proof tests for `ProvableCountIndexedTree` (PCIT).
//!
//! Coverage targets:
//! - `prove_count_indexed_top_k` / `verify_count_indexed_top_k`
//! - `prove_count_indexed_top_k_paginated` /
//!   `verify_count_indexed_top_k_paginated`
//! - `prove_count_indexed_count_range_aggregate` /
//!   `verify_count_indexed_count_range_aggregate`
//! - `prove_count_indexed_query` / `verify_count_indexed_query`
//!
//! These tests exercise the proof envelope construction and
//! verification paths in `grovedb/src/operations/proof/count_indexed.rs`
//! (~1700 LOC, was 0% covered).

#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::{query::QueryItem as MerkQueryItem, Query as MerkQuery};
    use grovedb_version::version::GroveVersion;

    use crate::{
        tests::{make_test_grovedb, TEST_LEAF},
        Element, Error, GroveDb,
    };

    // -----------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------

    /// Builds a PCIT at `[TEST_LEAF, cidx]` and inserts the given
    /// `(key, count)` pairs as `CountTree` entries with the specified
    /// count value.
    fn build_pcit_with_counts(
        db: &GroveDb,
        grove_version: &GroveVersion,
        entries: &[(&[u8], u64)],
    ) {
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PCIT");
        for (k, c) in entries {
            let ct = Element::new_provable_count_tree_with_flags_and_count_value(None, *c, None);
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                ct,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert PCIT entry");
        }
    }

    fn expected_root(db: &GroveDb, grove_version: &GroveVersion) -> [u8; 32] {
        db.root_hash(None, grove_version)
            .unwrap()
            .expect("root hash")
    }

    // -----------------------------------------------------------------
    // Happy-path round trips: prove_count_indexed_top_k
    // -----------------------------------------------------------------

    #[test]
    fn pcit_proof_top_k_descending_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(
            &db,
            grove_version,
            &[(b"alice", 5), (b"bob", 12), (b"carol", 1), (b"dave", 7)],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let proof = db
            .prove_count_indexed_top_k(path, 3, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_count_indexed_top_k(&proof, path, 3, true).expect("verify");
        // Descending top-3: bob(12), dave(7), alice(5).
        assert_eq!(
            result.entries,
            vec![
                (12u64, b"bob".to_vec()),
                (7u64, b"dave".to_vec()),
                (5u64, b"alice".to_vec()),
            ]
        );
        assert_eq!(result.root_hash, expected_root(&db, grove_version));
    }

    #[test]
    fn pcit_proof_top_k_ascending_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(
            &db,
            grove_version,
            &[(b"alice", 5), (b"bob", 12), (b"carol", 1), (b"dave", 7)],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let proof = db
            .prove_count_indexed_top_k(path, 3, false, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_count_indexed_top_k(&proof, path, 3, false).expect("verify");
        // Ascending top-3: carol(1), alice(5), dave(7).
        assert_eq!(
            result.entries,
            vec![
                (1u64, b"carol".to_vec()),
                (5u64, b"alice".to_vec()),
                (7u64, b"dave".to_vec()),
            ]
        );
        assert_eq!(result.root_hash, expected_root(&db, grove_version));
    }

    #[test]
    fn pcit_proof_top_k_k_equals_total_returns_all() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(&db, grove_version, &[(b"a", 1), (b"b", 2), (b"c", 3)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let proof = db
            .prove_count_indexed_top_k(path, 3, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_count_indexed_top_k(&proof, path, 3, true).expect("verify");
        assert_eq!(result.entries.len(), 3);
        assert_eq!(result.root_hash, expected_root(&db, grove_version));
    }

    #[test]
    fn pcit_proof_top_k_k_greater_than_total_returns_all() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(&db, grove_version, &[(b"a", 1), (b"b", 2)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let proof = db
            .prove_count_indexed_top_k(path, 100, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_count_indexed_top_k(&proof, path, 100, true).expect("verify");
        assert_eq!(result.entries.len(), 2);
        assert_eq!(result.root_hash, expected_root(&db, grove_version));
    }

    #[test]
    fn pcit_proof_top_k_k_one_returns_just_top() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(&db, grove_version, &[(b"a", 1), (b"b", 99), (b"c", 3)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let proof = db
            .prove_count_indexed_top_k(path, 1, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_count_indexed_top_k(&proof, path, 1, true).expect("verify");
        assert_eq!(result.entries, vec![(99u64, b"b".to_vec())]);
    }

    #[test]
    fn pcit_proof_top_k_k_zero_returns_empty() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(&db, grove_version, &[(b"a", 1), (b"b", 2), (b"c", 3)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let proof = db
            .prove_count_indexed_top_k(path, 0, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_count_indexed_top_k(&proof, path, 0, true).expect("verify");
        assert!(result.entries.is_empty());
        assert_eq!(result.root_hash, expected_root(&db, grove_version));
    }

    #[test]
    fn pcit_proof_top_k_on_empty_primary_errors() {
        // An empty cidx has no secondary content; the merk-level range
        // proof can't generate against an empty tree. The builder
        // surfaces the CorruptedData error.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create empty");
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let result = db
            .prove_count_indexed_top_k(path, 10, true, None, grove_version)
            .unwrap();
        assert!(
            matches!(result, Err(Error::CorruptedData(_))),
            "expected CorruptedData on empty cidx top_k prove, got {:?}",
            result
        );
    }

    // -----------------------------------------------------------------
    // verify_count_indexed_top_k: rejection paths
    // -----------------------------------------------------------------

    #[test]
    fn pcit_verify_top_k_rejects_direction_mismatch() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(&db, grove_version, &[(b"a", 1)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let proof = db
            .prove_count_indexed_top_k(path, 1, true, None, grove_version)
            .unwrap()
            .expect("prove descending");
        // Verify asking for ascending should fail.
        let err = GroveDb::verify_count_indexed_top_k(&proof, path, 1, false).unwrap_err();
        assert!(matches!(err, Error::CorruptedData(msg) if msg.contains("direction mismatch")));
    }

    #[test]
    fn pcit_verify_top_k_rejects_limit_mismatch() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(&db, grove_version, &[(b"a", 1), (b"b", 2)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let proof = db
            .prove_count_indexed_top_k(path, 2, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let err = GroveDb::verify_count_indexed_top_k(&proof, path, 5, true).unwrap_err();
        assert!(matches!(err, Error::CorruptedData(msg) if msg.contains("limit mismatch")));
    }

    #[test]
    fn pcit_verify_top_k_rejects_corrupt_bytes() {
        let result = GroveDb::verify_count_indexed_top_k(&[0xff; 5], &[b"x"], 1, true);
        assert!(matches!(result, Err(Error::CorruptedData(_))));
    }

    #[test]
    fn pcit_verify_top_k_rejects_truncated_proof() {
        let result = GroveDb::verify_count_indexed_top_k(&[0x00, 0x01], &[b"x"], 1, true);
        assert!(matches!(result, Err(Error::CorruptedData(_))));
    }

    #[test]
    fn pcit_verify_top_k_rejects_empty_bytes() {
        let result = GroveDb::verify_count_indexed_top_k(&[], &[b"x"], 1, true);
        assert!(matches!(result, Err(Error::CorruptedData(_))));
    }

    #[test]
    fn pcit_verify_top_k_rejects_path_length_mismatch() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(&db, grove_version, &[(b"a", 1), (b"b", 2)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let proof = db
            .prove_count_indexed_top_k(path, 2, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let wrong_path: &[&[u8]] = &[TEST_LEAF, b"cidx", b"extra"];
        let err = GroveDb::verify_count_indexed_top_k(&proof, wrong_path, 2, true).unwrap_err();
        assert!(matches!(err, Error::CorruptedData(msg) if msg.contains("layers but path has")));
    }

    #[test]
    fn pcit_verify_top_k_rejects_shorter_path() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(&db, grove_version, &[(b"a", 1)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let proof = db
            .prove_count_indexed_top_k(path, 1, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let shorter: &[&[u8]] = &[TEST_LEAF];
        let err = GroveDb::verify_count_indexed_top_k(&proof, shorter, 1, true).unwrap_err();
        assert!(matches!(err, Error::CorruptedData(_)));
    }

    #[test]
    fn pcit_prove_top_k_at_root_path_errors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let empty_path: &[&[u8]] = &[];
        let result = db
            .prove_count_indexed_top_k(empty_path, 3, true, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidPath(_))));
    }

    #[test]
    fn pcit_prove_top_k_on_non_cidx_target_errors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        // TEST_LEAF is a regular Tree, not a cidx.
        let result = db
            .prove_count_indexed_top_k([TEST_LEAF].as_ref(), 3, true, None, grove_version)
            .unwrap();
        match result {
            Err(Error::InvalidPath(msg)) => {
                assert!(
                    msg.contains("CountIndexedTree"),
                    "expected message to mention CountIndexedTree, got {msg}"
                );
            }
            other => panic!("expected InvalidPath, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------
    // Tampered proof bytes — detect manipulation
    // -----------------------------------------------------------------

    #[test]
    fn pcit_verify_top_k_rejects_tampered_secondary_proof() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(&db, grove_version, &[(b"a", 1), (b"b", 2), (b"c", 3)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let mut proof = db
            .prove_count_indexed_top_k(path, 3, true, None, grove_version)
            .unwrap()
            .expect("prove");
        // Flip a byte in the last ~64 bytes of the proof. Most envelope
        // formats hold the secondary proof at the tail; flipping a byte
        // there should break the secondary's root hash chain.
        let len = proof.len();
        proof[len - 16] ^= 0x01;
        let result = GroveDb::verify_count_indexed_top_k(&proof, path, 3, true);
        assert!(result.is_err(), "tampered proof must fail verification");
    }

    #[test]
    fn pcit_verify_top_k_rejects_tampered_middle_bytes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(&db, grove_version, &[(b"a", 1), (b"b", 2)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let mut proof = db
            .prove_count_indexed_top_k(path, 2, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let mid = proof.len() / 2;
        proof[mid] ^= 0xff;
        let result = GroveDb::verify_count_indexed_top_k(&proof, path, 2, true);
        assert!(result.is_err());
    }

    #[test]
    fn pcit_verify_top_k_against_wrong_path_segments() {
        // Honest proof at [TEST_LEAF, cidx_a], verifier called with
        // [TEST_LEAF, cidx_b]. Same depth, different last key.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx_a",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx_a");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx_b",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx_b");
        for (k, c) in [(b"x".as_ref(), 1u64), (b"y", 2)] {
            let ct = Element::new_provable_count_tree_with_flags_and_count_value(None, c, None);
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx_a"].as_ref(),
                k,
                ct,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        let path_a: &[&[u8]] = &[TEST_LEAF, b"cidx_a"];
        let proof = db
            .prove_count_indexed_top_k(path_a, 2, true, None, grove_version)
            .unwrap()
            .expect("prove cidx_a");
        let path_b: &[&[u8]] = &[TEST_LEAF, b"cidx_b"];
        // The single-key proof at depth 1 is for "cidx_a" but path
        // claims "cidx_b", so the layer chain check must fail.
        let result = GroveDb::verify_count_indexed_top_k(&proof, path_b, 2, true);
        assert!(result.is_err(), "wrong path must fail");
    }

    // -----------------------------------------------------------------
    // Nested cidx: outer/inner round trip — exercises last_idx > 0
    // and the secondary attestation chain.
    // -----------------------------------------------------------------

    #[test]
    fn pcit_proof_top_k_nested_cidx_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create outer cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer"].as_ref(),
            b"inner",
            Element::empty_provable_count_indexed_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("create inner cidx");
        for (k, c) in [(b"a".as_ref(), 5u64), (b"b", 10), (b"c", 1)] {
            let ct = Element::new_provable_count_tree_with_flags_and_count_value(None, c, None);
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"outer", b"inner"].as_ref(),
                k,
                ct,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert into inner");
        }
        let path: &[&[u8]] = &[TEST_LEAF, b"outer", b"inner"];
        let proof = db
            .prove_count_indexed_top_k(path, 5, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_count_indexed_top_k(&proof, path, 5, true).expect("verify");
        assert_eq!(result.entries.len(), 3);
        // Top entry: b(10).
        assert_eq!(result.entries[0], (10u64, b"b".to_vec()));
        assert_eq!(result.root_hash, expected_root(&db, grove_version));
    }

    #[test]
    fn pcit_proof_top_k_deeper_nesting_round_trip() {
        // 4 layers: TEST_LEAF / l1 / l2 / inner_cidx
        // Where l1 is a regular Tree and l2 is a PCIT (cidx ancestor).
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"l1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create l1");
        db.insert(
            [TEST_LEAF, b"l1"].as_ref(),
            b"l2",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create l2 cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"l1", b"l2"].as_ref(),
            b"inner_cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("create inner cidx");
        for (k, c) in [(b"x".as_ref(), 3u64), (b"y", 7), (b"z", 1)] {
            let ct = Element::new_provable_count_tree_with_flags_and_count_value(None, c, None);
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"l1", b"l2", b"inner_cidx"].as_ref(),
                k,
                ct,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        let path: &[&[u8]] = &[TEST_LEAF, b"l1", b"l2", b"inner_cidx"];
        let proof = db
            .prove_count_indexed_top_k(path, 10, true, None, grove_version)
            .unwrap()
            .expect("prove deeper");
        let result = GroveDb::verify_count_indexed_top_k(&proof, path, 10, true).expect("verify");
        assert_eq!(result.entries.len(), 3);
        assert_eq!(result.root_hash, expected_root(&db, grove_version));
    }

    // -----------------------------------------------------------------
    // Paginated proof tests
    // -----------------------------------------------------------------

    #[test]
    fn pcit_proof_paginated_descending_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(
            &db,
            grove_version,
            &[
                (b"alice", 5),
                (b"bob", 12),
                (b"carol", 1),
                (b"dave", 7),
                (b"eve", 20),
                (b"frank", 30),
            ],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        // Descending: frank(30), eve(20), bob(12), dave(7), alice(5),
        // carol(1). Paginated (k=2, offset=2) -> bob, dave.
        let proof = db
            .prove_count_indexed_top_k_paginated(path, 2, 2, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_count_indexed_top_k_paginated(&proof, path, 2, 2, true)
            .expect("verify");
        assert_eq!(
            result.entries,
            vec![(12u64, b"bob".to_vec()), (7u64, b"dave".to_vec())]
        );
        assert_eq!(result.skipped, 2);
        assert_eq!(result.root_hash, expected_root(&db, grove_version));
    }

    #[test]
    fn pcit_proof_paginated_ascending_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(
            &db,
            grove_version,
            &[(b"a", 1), (b"b", 2), (b"c", 3), (b"d", 4), (b"e", 5)],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        // Ascending paginated (k=2, offset=1): skip a(1), return b(2), c(3).
        let proof = db
            .prove_count_indexed_top_k_paginated(path, 2, 1, false, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_count_indexed_top_k_paginated(&proof, path, 2, 1, false)
            .expect("verify");
        assert_eq!(
            result.entries,
            vec![(2u64, b"b".to_vec()), (3u64, b"c".to_vec())]
        );
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn pcit_proof_paginated_offset_zero_matches_top_k() {
        // Offset 0 should give the same first-k entries as the non-paginated
        // top_k proof.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(
            &db,
            grove_version,
            &[(b"a", 1), (b"b", 2), (b"c", 3), (b"d", 4)],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let top_proof = db
            .prove_count_indexed_top_k(path, 3, true, None, grove_version)
            .unwrap()
            .expect("top_k");
        let pag_proof = db
            .prove_count_indexed_top_k_paginated(path, 3, 0, true, None, grove_version)
            .unwrap()
            .expect("paginated");
        let top_result =
            GroveDb::verify_count_indexed_top_k(&top_proof, path, 3, true).expect("verify");
        let pag_result =
            GroveDb::verify_count_indexed_top_k_paginated(&pag_proof, path, 3, 0, true)
                .expect("verify");
        assert_eq!(top_result.entries, pag_result.entries);
        assert_eq!(pag_result.skipped, 0);
    }

    #[test]
    fn pcit_proof_paginated_offset_past_end_returns_empty() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(&db, grove_version, &[(b"a", 1), (b"b", 2), (b"c", 3)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let proof = db
            .prove_count_indexed_top_k_paginated(path, 5, 100, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_count_indexed_top_k_paginated(&proof, path, 5, 100, true)
            .expect("verify");
        assert!(result.entries.is_empty());
    }

    #[test]
    fn pcit_proof_paginated_empty_primary() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let proof = db
            .prove_count_indexed_top_k_paginated(path, 3, 0, true, None, grove_version)
            .unwrap()
            .expect("prove on empty");
        let result = GroveDb::verify_count_indexed_top_k_paginated(&proof, path, 3, 0, true)
            .expect("verify on empty");
        assert!(result.entries.is_empty());
        assert_eq!(result.root_hash, expected_root(&db, grove_version));
    }

    #[test]
    fn pcit_proof_paginated_spans_page_boundary() {
        // 10 entries, page size 3. Pages: (0-2, 3-5, 6-8, 9).
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let entries: Vec<(Vec<u8>, u64)> = (0..10u64).map(|i| (vec![b'k', i as u8], i)).collect();
        let entry_refs: Vec<(&[u8], u64)> =
            entries.iter().map(|(k, c)| (k.as_slice(), *c)).collect();
        build_pcit_with_counts(&db, grove_version, &entry_refs);
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        // page 2: offset=6, k=3 → counts 3, 2, 1 (ascending top-9, skip 6).
        let proof = db
            .prove_count_indexed_top_k_paginated(path, 3, 6, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_count_indexed_top_k_paginated(&proof, path, 3, 6, true)
            .expect("verify");
        // Descending order: 9..0 — skip first 6 (9,8,7,6,5,4), return 3,2,1.
        assert_eq!(result.entries.len(), 3);
        assert_eq!(result.entries[0].0, 3);
        assert_eq!(result.entries[1].0, 2);
        assert_eq!(result.entries[2].0, 1);
        assert_eq!(result.skipped, 6);
    }

    #[test]
    fn pcit_verify_paginated_rejects_k_mismatch() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(&db, grove_version, &[(b"a", 1)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let proof = db
            .prove_count_indexed_top_k_paginated(path, 2, 0, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let err =
            GroveDb::verify_count_indexed_top_k_paginated(&proof, path, 3, 0, true).unwrap_err();
        assert!(matches!(err, Error::CorruptedData(msg) if msg.contains("k mismatch")));
    }

    #[test]
    fn pcit_verify_paginated_rejects_offset_mismatch() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(&db, grove_version, &[(b"a", 1)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let proof = db
            .prove_count_indexed_top_k_paginated(path, 2, 1, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let err =
            GroveDb::verify_count_indexed_top_k_paginated(&proof, path, 2, 0, true).unwrap_err();
        assert!(matches!(err, Error::CorruptedData(msg) if msg.contains("offset mismatch")));
    }

    #[test]
    fn pcit_verify_paginated_rejects_direction_mismatch() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(&db, grove_version, &[(b"a", 1)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let proof = db
            .prove_count_indexed_top_k_paginated(path, 2, 0, true, None, grove_version)
            .unwrap()
            .expect("prove descending");
        let err =
            GroveDb::verify_count_indexed_top_k_paginated(&proof, path, 2, 0, false).unwrap_err();
        assert!(matches!(err, Error::CorruptedData(msg) if msg.contains("direction mismatch")));
    }

    #[test]
    fn pcit_verify_paginated_rejects_corrupt_bytes() {
        let result = GroveDb::verify_count_indexed_top_k_paginated(&[0xff; 4], &[b"x"], 1, 0, true);
        assert!(matches!(result, Err(Error::CorruptedData(_))));
    }

    #[test]
    fn pcit_verify_paginated_rejects_path_length_mismatch() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(&db, grove_version, &[(b"a", 1)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let proof = db
            .prove_count_indexed_top_k_paginated(path, 1, 0, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let wrong: &[&[u8]] = &[TEST_LEAF, b"cidx", b"extra"];
        let err =
            GroveDb::verify_count_indexed_top_k_paginated(&proof, wrong, 1, 0, true).unwrap_err();
        assert!(matches!(err, Error::CorruptedData(msg) if msg.contains("layers but path has")));
    }

    #[test]
    fn pcit_prove_paginated_at_root_path_errors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let empty_path: &[&[u8]] = &[];
        let result = db
            .prove_count_indexed_top_k_paginated(empty_path, 3, 0, true, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidPath(_))));
    }

    #[test]
    fn pcit_prove_paginated_on_non_cidx_target_errors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let result = db
            .prove_count_indexed_top_k_paginated(
                [TEST_LEAF].as_ref(),
                3,
                0,
                true,
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidPath(_))));
    }

    #[test]
    fn pcit_proof_paginated_nested_cidx() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create outer");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer"].as_ref(),
            b"inner",
            Element::empty_provable_count_indexed_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("create inner");
        for (k, c) in [(b"a".as_ref(), 1u64), (b"b", 2), (b"c", 3), (b"d", 4)] {
            let ct = Element::new_provable_count_tree_with_flags_and_count_value(None, c, None);
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"outer", b"inner"].as_ref(),
                k,
                ct,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        let path: &[&[u8]] = &[TEST_LEAF, b"outer", b"inner"];
        let proof = db
            .prove_count_indexed_top_k_paginated(path, 2, 1, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_count_indexed_top_k_paginated(&proof, path, 2, 1, true)
            .expect("verify");
        // Descending: d(4), c(3), b(2), a(1). Skip 1 (d), take 2: c, b.
        assert_eq!(
            result.entries,
            vec![(3u64, b"c".to_vec()), (2u64, b"b".to_vec())]
        );
        assert_eq!(result.skipped, 1);
        assert_eq!(result.root_hash, expected_root(&db, grove_version));
    }

    // -----------------------------------------------------------------
    // Aggregate-count proof tests
    // -----------------------------------------------------------------

    #[test]
    fn pcit_proof_aggregate_count_full_range_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(
            &db,
            grove_version,
            &[(b"a", 1), (b"b", 5), (b"c", 10), (b"d", 20), (b"e", 100)],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let proof = db
            .prove_count_indexed_count_range_aggregate(path, 0, u64::MAX, None, grove_version)
            .unwrap()
            .expect("prove full range aggregate");
        let result = GroveDb::verify_count_indexed_count_range_aggregate(&proof, path, 0, u64::MAX)
            .expect("verify");
        assert_eq!(result.count, 5);
        assert_eq!(result.root_hash, expected_root(&db, grove_version));
    }

    #[test]
    fn pcit_proof_aggregate_count_partial_range() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(
            &db,
            grove_version,
            &[(b"a", 1), (b"b", 5), (b"c", 10), (b"d", 20), (b"e", 100)],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        // count in [5, 20] → b(5), c(10), d(20) = 3 entries.
        let proof = db
            .prove_count_indexed_count_range_aggregate(path, 5, 20, None, grove_version)
            .unwrap()
            .expect("prove range");
        let result = GroveDb::verify_count_indexed_count_range_aggregate(&proof, path, 5, 20)
            .expect("verify");
        assert_eq!(result.count, 3);
    }

    #[test]
    fn pcit_proof_aggregate_count_exact_match() {
        // lo == hi: single value match.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(&db, grove_version, &[(b"a", 5), (b"b", 5), (b"c", 10)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let proof = db
            .prove_count_indexed_count_range_aggregate(path, 5, 5, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_count_indexed_count_range_aggregate(&proof, path, 5, 5)
            .expect("verify");
        assert_eq!(result.count, 2);
    }

    #[test]
    fn pcit_proof_aggregate_count_degenerate_lo_greater_than_hi() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(&db, grove_version, &[(b"a", 5), (b"b", 10)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        // lo > hi → degenerate, count must be 0.
        let proof = db
            .prove_count_indexed_count_range_aggregate(path, 100, 50, None, grove_version)
            .unwrap()
            .expect("prove degenerate");
        let result = GroveDb::verify_count_indexed_count_range_aggregate(&proof, path, 100, 50)
            .expect("verify");
        assert_eq!(result.count, 0);
        assert_eq!(result.root_hash, expected_root(&db, grove_version));
    }

    #[test]
    fn pcit_proof_aggregate_count_no_matches() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(&db, grove_version, &[(b"a", 1), (b"b", 2)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        // Range [100, 200] doesn't match anything.
        let proof = db
            .prove_count_indexed_count_range_aggregate(path, 100, 200, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_count_indexed_count_range_aggregate(&proof, path, 100, 200)
            .expect("verify");
        assert_eq!(result.count, 0);
    }

    #[test]
    fn pcit_proof_aggregate_count_empty_primary() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let proof = db
            .prove_count_indexed_count_range_aggregate(path, 0, u64::MAX, None, grove_version)
            .unwrap()
            .expect("prove empty");
        let result = GroveDb::verify_count_indexed_count_range_aggregate(&proof, path, 0, u64::MAX)
            .expect("verify");
        assert_eq!(result.count, 0);
        assert_eq!(result.root_hash, expected_root(&db, grove_version));
    }

    #[test]
    fn pcit_proof_aggregate_count_with_hi_u64_max() {
        // Exercise the RangeFrom branch in the builder.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(&db, grove_version, &[(b"a", 5), (b"b", 100), (b"c", 1000)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let proof = db
            .prove_count_indexed_count_range_aggregate(path, 50, u64::MAX, None, grove_version)
            .unwrap()
            .expect("prove range_from");
        let result =
            GroveDb::verify_count_indexed_count_range_aggregate(&proof, path, 50, u64::MAX)
                .expect("verify");
        assert_eq!(result.count, 2); // b(100), c(1000).
    }

    #[test]
    fn pcit_verify_aggregate_count_rejects_lo_mismatch() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(&db, grove_version, &[(b"a", 5)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let proof = db
            .prove_count_indexed_count_range_aggregate(path, 0, 10, None, grove_version)
            .unwrap()
            .expect("prove");
        let err =
            GroveDb::verify_count_indexed_count_range_aggregate(&proof, path, 1, 10).unwrap_err();
        assert!(matches!(err, Error::CorruptedData(msg) if msg.contains("lo_count mismatch")));
    }

    #[test]
    fn pcit_verify_aggregate_count_rejects_hi_mismatch() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(&db, grove_version, &[(b"a", 5)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let proof = db
            .prove_count_indexed_count_range_aggregate(path, 0, 10, None, grove_version)
            .unwrap()
            .expect("prove");
        let err =
            GroveDb::verify_count_indexed_count_range_aggregate(&proof, path, 0, 11).unwrap_err();
        assert!(matches!(err, Error::CorruptedData(msg) if msg.contains("hi_count mismatch")));
    }

    #[test]
    fn pcit_verify_aggregate_count_rejects_corrupt_bytes() {
        let result =
            GroveDb::verify_count_indexed_count_range_aggregate(&[0xff; 4], &[b"x"], 0, 10);
        assert!(matches!(result, Err(Error::CorruptedData(_))));
    }

    #[test]
    fn pcit_verify_aggregate_count_rejects_path_length_mismatch() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(&db, grove_version, &[(b"a", 5)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let proof = db
            .prove_count_indexed_count_range_aggregate(path, 0, 10, None, grove_version)
            .unwrap()
            .expect("prove");
        let wrong: &[&[u8]] = &[TEST_LEAF];
        let err =
            GroveDb::verify_count_indexed_count_range_aggregate(&proof, wrong, 0, 10).unwrap_err();
        assert!(matches!(err, Error::CorruptedData(_)));
    }

    #[test]
    fn pcit_prove_aggregate_count_at_root_path_errors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let empty: &[&[u8]] = &[];
        let result = db
            .prove_count_indexed_count_range_aggregate(empty, 0, 10, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidPath(_))));
    }

    #[test]
    fn pcit_prove_aggregate_count_on_non_cidx_target_errors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let result = db
            .prove_count_indexed_count_range_aggregate(
                [TEST_LEAF].as_ref(),
                0,
                10,
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidPath(_))));
    }

    #[test]
    fn pcit_proof_aggregate_count_nested_cidx() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create outer");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer"].as_ref(),
            b"inner",
            Element::empty_provable_count_indexed_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("create inner");
        for (k, c) in [(b"a".as_ref(), 5u64), (b"b", 10), (b"c", 50)] {
            let ct = Element::new_provable_count_tree_with_flags_and_count_value(None, c, None);
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"outer", b"inner"].as_ref(),
                k,
                ct,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        let path: &[&[u8]] = &[TEST_LEAF, b"outer", b"inner"];
        let proof = db
            .prove_count_indexed_count_range_aggregate(path, 0, u64::MAX, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_count_indexed_count_range_aggregate(&proof, path, 0, u64::MAX)
            .expect("verify");
        assert_eq!(result.count, 3);
        assert_eq!(result.root_hash, expected_root(&db, grove_version));
    }

    #[test]
    fn pcit_verify_aggregate_count_rejects_tampered_proof() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(&db, grove_version, &[(b"a", 1), (b"b", 2), (b"c", 3)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let mut proof = db
            .prove_count_indexed_count_range_aggregate(path, 0, 10, None, grove_version)
            .unwrap()
            .expect("prove");
        let len = proof.len();
        proof[len - 8] ^= 0x01;
        let result = GroveDb::verify_count_indexed_count_range_aggregate(&proof, path, 0, 10);
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------
    // prove_count_indexed_query / verify_count_indexed_query
    // -----------------------------------------------------------------

    #[test]
    fn pcit_proof_query_full_range_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(&db, grove_version, &[(b"a", 1), (b"b", 2), (b"c", 3)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let mut q = MerkQuery::new();
        q.insert_all();
        q.left_to_right = true; // ascending
        let proof = db
            .prove_count_indexed_query(path, q.clone(), None, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_count_indexed_query(&proof, q, None, path).expect("verify");
        assert_eq!(result.entries.len(), 3);
        assert_eq!(result.root_hash, expected_root(&db, grove_version));
    }

    #[test]
    fn pcit_proof_query_with_limit_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(
            &db,
            grove_version,
            &[(b"a", 1), (b"b", 2), (b"c", 3), (b"d", 4)],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let mut q = MerkQuery::new();
        q.insert_all();
        q.left_to_right = false; // descending
        let proof = db
            .prove_count_indexed_query(path, q.clone(), Some(2), None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_count_indexed_query(&proof, q, Some(2), path).expect("verify");
        assert_eq!(result.entries.len(), 2);
        // Top 2 descending: d(4), c(3).
        assert_eq!(result.entries[0].0, 4);
        assert_eq!(result.entries[1].0, 3);
    }

    #[test]
    fn pcit_proof_query_specific_range_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(
            &db,
            grove_version,
            &[(b"a", 1), (b"b", 5), (b"c", 10), (b"d", 20)],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        // Range [5, 11) on count_value (encoded as 8-byte BE).
        let mut q = MerkQuery::new();
        let lo = 5u64.to_be_bytes().to_vec();
        let hi = 11u64.to_be_bytes().to_vec();
        q.insert_range(lo..hi);
        let proof = db
            .prove_count_indexed_query(path, q.clone(), None, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_count_indexed_query(&proof, q, None, path).expect("verify");
        // count_value 5 (b) and 10 (c) are in [5,11).
        assert_eq!(result.entries.len(), 2);
    }

    #[test]
    fn pcit_verify_query_rejects_limit_mismatch() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(&db, grove_version, &[(b"a", 1), (b"b", 2)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let mut q = MerkQuery::new();
        q.insert_all();
        let proof = db
            .prove_count_indexed_query(path, q.clone(), Some(1), None, grove_version)
            .unwrap()
            .expect("prove");
        let err = GroveDb::verify_count_indexed_query(&proof, q, Some(2), path).unwrap_err();
        assert!(matches!(err, Error::CorruptedData(msg) if msg.contains("limit mismatch")));
    }

    #[test]
    fn pcit_verify_query_rejects_none_vs_some_zero_limit_mismatch() {
        // None and Some(0) are distinct in the envelope; check the
        // verifier enforces this.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(&db, grove_version, &[(b"a", 1)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let mut q = MerkQuery::new();
        q.insert_all();
        let proof = db
            .prove_count_indexed_query(path, q.clone(), None, None, grove_version)
            .unwrap()
            .expect("prove");
        let err = GroveDb::verify_count_indexed_query(&proof, q, Some(0), path).unwrap_err();
        assert!(matches!(err, Error::CorruptedData(_)));
    }

    #[test]
    fn pcit_verify_query_rejects_direction_mismatch_via_query() {
        // The envelope binds the descending direction; if the caller
        // submits a query with the opposite left_to_right, the verifier
        // rejects.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(&db, grove_version, &[(b"a", 1), (b"b", 2)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let mut q_asc = MerkQuery::new();
        q_asc.insert_all();
        q_asc.left_to_right = true;
        let proof = db
            .prove_count_indexed_query(path, q_asc.clone(), Some(2), None, grove_version)
            .unwrap()
            .expect("prove");
        let mut q_desc = MerkQuery::new();
        q_desc.insert_all();
        q_desc.left_to_right = false;
        let err = GroveDb::verify_count_indexed_query(&proof, q_desc, Some(2), path).unwrap_err();
        assert!(matches!(err, Error::CorruptedData(_)));
    }

    #[test]
    fn pcit_verify_query_rejects_corrupt_bytes() {
        let mut q = MerkQuery::new();
        q.insert_all();
        let result = GroveDb::verify_count_indexed_query(&[0xff; 5], q, None, &[b"x"]);
        assert!(matches!(result, Err(Error::CorruptedData(_))));
    }

    #[test]
    fn pcit_prove_query_on_non_cidx_target_errors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let mut q = MerkQuery::new();
        q.insert_all();
        let result = db
            .prove_count_indexed_query([TEST_LEAF].as_ref(), q, None, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidPath(_))));
    }

    #[test]
    fn pcit_prove_query_at_root_path_errors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let mut q = MerkQuery::new();
        q.insert_all();
        let empty: &[&[u8]] = &[];
        let result = db
            .prove_count_indexed_query(empty, q, None, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidPath(_))));
    }

    #[test]
    fn pcit_proof_query_single_key_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(&db, grove_version, &[(b"alice", 5), (b"bob", 12)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        // Query the exact secondary key for alice (count_be ‖ "alice").
        let mut alice_key = 5u64.to_be_bytes().to_vec();
        alice_key.extend_from_slice(b"alice");
        let mut q = MerkQuery::new();
        q.insert_item(MerkQueryItem::Key(alice_key));
        let proof = db
            .prove_count_indexed_query(path, q.clone(), None, None, grove_version)
            .unwrap()
            .expect("prove single key");
        let result = GroveDb::verify_count_indexed_query(&proof, q, None, path).expect("verify");
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0], (5u64, b"alice".to_vec()));
    }

    // -----------------------------------------------------------------
    // After mutations: proofs reflect updated state
    // -----------------------------------------------------------------

    #[test]
    fn pcit_proof_top_k_reflects_post_delete_state() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(&db, grove_version, &[(b"a", 1), (b"b", 2), (b"c", 3)]);
        db.delete_from_count_indexed_tree([TEST_LEAF, b"cidx"].as_ref(), b"b", None, grove_version)
            .unwrap()
            .expect("delete b");
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let proof = db
            .prove_count_indexed_top_k(path, 5, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_count_indexed_top_k(&proof, path, 5, true).expect("verify");
        assert_eq!(result.entries.len(), 2);
        // c(3) and a(1) remain.
        assert_eq!(result.entries[0].1, b"c".to_vec());
        assert_eq!(result.entries[1].1, b"a".to_vec());
    }

    #[test]
    fn pcit_proof_top_k_root_hash_matches_db_root_hash() {
        // Verify the verifier-reconstructed root matches db.root_hash
        // even after a number of inserts.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(
            &db,
            grove_version,
            &[
                (b"k1", 11),
                (b"k2", 22),
                (b"k3", 33),
                (b"k4", 44),
                (b"k5", 55),
            ],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let proof = db
            .prove_count_indexed_top_k(path, 5, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_count_indexed_top_k(&proof, path, 5, true).expect("verify");
        let trusted_root = expected_root(&db, grove_version);
        assert_eq!(result.root_hash, trusted_root);
    }

    #[test]
    fn pcit_proof_aggregate_count_consistent_with_non_proven_query() {
        // The proof's reported count should match the underlying
        // (non-proven) query.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit_with_counts(
            &db,
            grove_version,
            &[
                (b"a", 1),
                (b"b", 3),
                (b"c", 5),
                (b"d", 7),
                (b"e", 9),
                (b"f", 11),
            ],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let unproven = db
            .indexed_count_range_aggregate(path, 3, 9, None, grove_version)
            .unwrap()
            .expect("unproven");
        let proof = db
            .prove_count_indexed_count_range_aggregate(path, 3, 9, None, grove_version)
            .unwrap()
            .expect("prove");
        let proven = GroveDb::verify_count_indexed_count_range_aggregate(&proof, path, 3, 9)
            .expect("verify");
        assert_eq!(unproven, proven.count);
    }

    // -----------------------------------------------------------------
    // Many-entry test to exercise larger merk proofs
    // -----------------------------------------------------------------

    #[test]
    fn pcit_proof_top_k_many_entries() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        // Insert 30 entries with distinct counts.
        let entries: Vec<(Vec<u8>, u64)> = (0..30u64)
            .map(|i| (format!("k{:02}", i).into_bytes(), i))
            .collect();
        let refs: Vec<(&[u8], u64)> = entries.iter().map(|(k, c)| (k.as_slice(), *c)).collect();
        build_pcit_with_counts(&db, grove_version, &refs);
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let proof = db
            .prove_count_indexed_top_k(path, 10, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_count_indexed_top_k(&proof, path, 10, true).expect("verify");
        assert_eq!(result.entries.len(), 10);
        // Top entries descending: 29, 28, 27...
        assert_eq!(result.entries[0].0, 29);
        assert_eq!(result.entries[9].0, 20);
    }
}
