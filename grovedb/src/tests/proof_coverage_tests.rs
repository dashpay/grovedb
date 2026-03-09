//! Targeted coverage tests for proof verify/generate/mod/util.
//!
//! These tests exercise uncovered lines in:
//! - `operations/proof/mod.rs` — Display impls, ProveOptions, GroveDBProof methods
//! - `operations/proof/util.rs` — hex_to_ascii, path_hex_to_ascii, Display impls
//! - `operations/proof/verify.rs` — v1 verification paths, chained queries,
//!   subset verification, corrupt proof detection, absence proofs on v1
//! - `operations/proof/generate.rs` — v0 error for non-Merk trees, v1 proof
//!   generation with limits, prove_query_non_serialized edge cases

#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::{
        query::{QueryItem, SubqueryBranch, VerifyOptions},
        Query,
    };
    use grovedb_version::version::{v2::GROVE_V2, GroveVersion};
    use indexmap::IndexMap;

    use crate::{
        operations::proof::{
            util::{
                hex_to_ascii, path_as_slices_hex_to_ascii, path_hex_to_ascii,
                ProvedPathKeyOptionalValue, ProvedPathKeyValue,
            },
            GroveDBProof, ProveOptions,
        },
        tests::{
            common::EMPTY_PATH, make_deep_tree, make_empty_grovedb, make_test_grovedb, TEST_LEAF,
        },
        Element, GroveDb, PathQuery, SizedQuery,
    };

    // =========================================================================
    // 1. operations/proof/util.rs coverage
    // =========================================================================

    #[test]
    fn hex_to_ascii_with_ascii_bytes() {
        // All bytes are in the allowed character set -> returns readable string
        let input = b"hello_world";
        let result = hex_to_ascii(input);
        assert_eq!(result, "hello_world", "should return readable ASCII string");
    }

    #[test]
    fn hex_to_ascii_with_non_ascii_bytes() {
        // Contains bytes outside the allowed set -> returns hex representation
        let input = &[0xFF, 0x00, 0xAB];
        let result = hex_to_ascii(input);
        assert!(
            result.starts_with("0x"),
            "non-ASCII bytes should be hex-encoded with 0x prefix"
        );
        assert_eq!(result, "0xff00ab");
    }

    #[test]
    fn hex_to_ascii_with_empty_bytes() {
        let input = b"";
        let result = hex_to_ascii(input);
        // Empty string is valid UTF-8 and all (zero) chars are in the allowed set
        assert_eq!(result, "", "empty bytes should produce empty string");
    }

    #[test]
    fn hex_to_ascii_with_mixed_allowed_chars() {
        // Test all allowed character classes: uppercase, lowercase, digits, special
        let input = b"ABC_xyz-012/[test]@";
        let result = hex_to_ascii(input);
        assert_eq!(
            result, "ABC_xyz-012/[test]@",
            "all allowed chars should be preserved"
        );
    }

    #[test]
    fn hex_to_ascii_with_space_is_hex() {
        // Space character is NOT in the allowed set
        let input = b"hello world";
        let result = hex_to_ascii(input);
        assert!(
            result.starts_with("0x"),
            "space is not an allowed char, should return hex"
        );
    }

    #[test]
    fn path_hex_to_ascii_with_multiple_segments() {
        let path: Vec<Vec<u8>> = vec![b"root".to_vec(), b"child".to_vec(), b"leaf".to_vec()];
        let result = path_hex_to_ascii(&path);
        assert_eq!(
            result, "root/child/leaf",
            "path segments should be joined with /"
        );
    }

    #[test]
    fn path_hex_to_ascii_with_hex_segment() {
        let path: Vec<Vec<u8>> = vec![b"root".to_vec(), vec![0xFF, 0x01]];
        let result = path_hex_to_ascii(&path);
        assert_eq!(result, "root/0xff01", "non-ASCII segment should be hex");
    }

    #[test]
    fn path_hex_to_ascii_empty_path() {
        let path: Vec<Vec<u8>> = vec![];
        let result = path_hex_to_ascii(&path);
        assert_eq!(result, "", "empty path should produce empty string");
    }

    #[test]
    fn path_as_slices_hex_to_ascii_basic() {
        let path: &[&[u8]] = &[b"segment1", b"segment2"];
        let result = path_as_slices_hex_to_ascii(path);
        assert_eq!(result, "segment1/segment2");
    }

    #[test]
    fn path_as_slices_hex_to_ascii_with_binary() {
        let binary: &[u8] = &[0xDE, 0xAD];
        let path: &[&[u8]] = &[b"prefix", binary];
        let result = path_as_slices_hex_to_ascii(path);
        assert_eq!(result, "prefix/0xdead");
    }

    #[test]
    fn proved_path_key_value_display() {
        let grove_version = GroveVersion::latest();
        let item = Element::new_item(b"data".to_vec());
        let serialized = item
            .serialize(grove_version)
            .expect("should serialize item");

        let proved = ProvedPathKeyValue {
            path: vec![b"root".to_vec(), b"child".to_vec()],
            key: b"mykey".to_vec(),
            value: serialized,
            proof: [0u8; 32],
        };
        let display = format!("{}", proved);
        assert!(
            display.contains("mykey"),
            "display should contain the key: {}",
            display
        );
        assert!(
            display.contains("root"),
            "display should contain path segment: {}",
            display
        );
    }

    #[test]
    fn proved_path_key_optional_value_display_some() {
        let grove_version = GroveVersion::latest();
        let item = Element::new_item(b"data".to_vec());
        let serialized = item
            .serialize(grove_version)
            .expect("should serialize item");

        let proved = ProvedPathKeyOptionalValue {
            path: vec![b"a".to_vec()],
            key: b"k".to_vec(),
            value: Some(serialized),
            proof: [1u8; 32],
        };
        let display = format!("{}", proved);
        assert!(
            display.contains("ProvedPathKeyValue"),
            "should contain type name"
        );
    }

    #[test]
    fn proved_path_key_optional_value_display_none() {
        let proved = ProvedPathKeyOptionalValue {
            path: vec![b"a".to_vec()],
            key: b"k".to_vec(),
            value: None,
            proof: [0u8; 32],
        };
        let display = format!("{}", proved);
        assert!(display.contains("None"), "should contain None for value");
    }

    #[test]
    fn proved_path_key_value_try_from_optional_some() {
        let proved_optional = ProvedPathKeyOptionalValue {
            path: vec![b"p".to_vec()],
            key: b"k".to_vec(),
            value: Some(vec![1, 2, 3]),
            proof: [0u8; 32],
        };
        let proved: ProvedPathKeyValue = proved_optional
            .try_into()
            .expect("should convert Some to ProvedPathKeyValue");
        assert_eq!(proved.value, vec![1, 2, 3]);
    }

    #[test]
    fn proved_path_key_value_try_from_optional_none_errors() {
        let proved_optional = ProvedPathKeyOptionalValue {
            path: vec![b"p".to_vec()],
            key: b"mykey".to_vec(),
            value: None,
            proof: [0u8; 32],
        };
        let result: Result<ProvedPathKeyValue, _> = proved_optional.try_into();
        assert!(
            result.is_err(),
            "converting None value should fail with InvalidProofError"
        );
    }

    #[test]
    fn proved_path_key_value_from_single() {
        use grovedb_merk::proofs::query::ProvedKeyValue;
        let pkv = ProvedKeyValue {
            key: b"key".to_vec(),
            value: vec![10, 20],
            proof: [5u8; 32],
        };
        let path = vec![b"root".to_vec()];
        let result = ProvedPathKeyValue::from_proved_key_value(path.clone(), pkv);
        assert_eq!(result.path, path);
        assert_eq!(result.key, b"key".to_vec());
        assert_eq!(result.value, vec![10, 20]);
    }

    #[test]
    fn proved_path_key_value_from_multiple() {
        use grovedb_merk::proofs::query::ProvedKeyValue;
        let pkvs = vec![
            ProvedKeyValue {
                key: b"a".to_vec(),
                value: vec![1],
                proof: [0u8; 32],
            },
            ProvedKeyValue {
                key: b"b".to_vec(),
                value: vec![2],
                proof: [1u8; 32],
            },
        ];
        let path = vec![b"p".to_vec()];
        let results = ProvedPathKeyValue::from_proved_key_values(path.clone(), pkvs);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].key, b"a".to_vec());
        assert_eq!(results[1].key, b"b".to_vec());
    }

    // =========================================================================
    // 2. operations/proof/mod.rs coverage
    // =========================================================================

    #[test]
    fn prove_options_display() {
        let opts = ProveOptions {
            decrease_limit_on_empty_sub_query_result: true,
        };
        let display = format!("{}", opts);
        assert!(
            display.contains("decrease_limit_on_empty_sub_query_result: true"),
            "should display the field: {}",
            display
        );

        let opts_false = ProveOptions {
            decrease_limit_on_empty_sub_query_result: false,
        };
        let display_false = format!("{}", opts_false);
        assert!(
            display_false.contains("false"),
            "should display false: {}",
            display_false
        );
    }

    #[test]
    fn prove_options_default() {
        let opts = ProveOptions::default();
        assert!(
            opts.decrease_limit_on_empty_sub_query_result,
            "default should have decrease_limit_on_empty_sub_query_result = true"
        );
    }

    #[test]
    fn grovedb_proof_verify_method_on_v0_proof() {
        // Exercise GroveDBProof::verify (the method on the decoded proof struct)
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"item1",
            Element::new_item(b"val1".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        let mut query = Query::new();
        query.insert_key(b"item1".to_vec());
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        // Generate proof and decode it to GroveDBProof
        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should generate proof");

        let config = bincode::config::standard()
            .with_big_endian()
            .with_limit::<{ 256 * 1024 * 1024 }>();
        let (grovedb_proof, _): (GroveDBProof, _) =
            bincode::decode_from_slice(&proof_bytes, config).expect("should decode proof");

        // Use the verify method on the proof struct
        let (root_hash, results) = grovedb_proof
            .verify(&path_query, grove_version)
            .expect("should verify via GroveDBProof::verify");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root, "root hash should match");
        assert_eq!(results.len(), 1, "should have 1 result");
    }

    #[test]
    fn grovedb_proof_verify_with_options_method() {
        // Exercise GroveDBProof::verify_with_options
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"opt1",
            Element::new_item(b"ov1".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert");

        let mut query = Query::new();
        query.insert_all();
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove");

        let config = bincode::config::standard()
            .with_big_endian()
            .with_limit::<{ 256 * 1024 * 1024 }>();
        let (grovedb_proof, _): (GroveDBProof, _) =
            bincode::decode_from_slice(&proof_bytes, config).expect("should decode");

        let (root_hash, results) = grovedb_proof
            .verify_with_options(
                &path_query,
                VerifyOptions {
                    absence_proofs_for_non_existing_searched_keys: false,
                    verify_proof_succinctness: false,
                    include_empty_trees_in_result: true,
                },
                grove_version,
            )
            .expect("should verify with options");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert!(
            !results.is_empty(),
            "should have results with include_empty_trees"
        );
    }

    #[test]
    fn grovedb_proof_verify_raw_method() {
        // Exercise GroveDBProof::verify_raw
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"raw1",
            Element::new_item(b"rv1".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert");

        let mut query = Query::new();
        query.insert_all();
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove");

        let config = bincode::config::standard()
            .with_big_endian()
            .with_limit::<{ 256 * 1024 * 1024 }>();
        let (grovedb_proof, _): (GroveDBProof, _) =
            bincode::decode_from_slice(&proof_bytes, config).expect("should decode");

        let (root_hash, raw_results) = grovedb_proof
            .verify_raw(&path_query, grove_version)
            .expect("verify_raw should succeed");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert!(!raw_results.is_empty(), "should have raw results");
    }

    #[test]
    fn grovedb_proof_verify_with_absence_proof_method() {
        // Exercise GroveDBProof::verify_with_absence_proof
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"exists",
            Element::new_item(b"val".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert");

        let mut query = Query::new();
        query.insert_key(b"exists".to_vec());
        query.insert_key(b"missing".to_vec());
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec()],
            SizedQuery::new(query, Some(2), None),
        );

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove");

        let config = bincode::config::standard()
            .with_big_endian()
            .with_limit::<{ 256 * 1024 * 1024 }>();
        let (grovedb_proof, _): (GroveDBProof, _) =
            bincode::decode_from_slice(&proof_bytes, config).expect("should decode");

        let (root_hash, results) = grovedb_proof
            .verify_with_absence_proof(&path_query, grove_version)
            .expect("verify_with_absence_proof should succeed");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(
            results.len(),
            2,
            "should have 2 results (1 present, 1 absent)"
        );
    }

    #[test]
    fn grovedb_proof_verify_subset_method() {
        // Exercise GroveDBProof::verify_subset
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        for i in 0..5u8 {
            db.insert(
                [TEST_LEAF].as_ref(),
                &[i],
                Element::new_item(vec![i + 10]),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert");
        }

        let mut full_query = Query::new();
        full_query.insert_all();
        let full_pq = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], full_query);

        let proof_bytes = db
            .prove_query(&full_pq, None, grove_version)
            .unwrap()
            .expect("should prove");

        let config = bincode::config::standard()
            .with_big_endian()
            .with_limit::<{ 256 * 1024 * 1024 }>();
        let (grovedb_proof, _): (GroveDBProof, _) =
            bincode::decode_from_slice(&proof_bytes, config).expect("should decode");

        // Verify with a subset query for just 2 keys
        let mut subset_query = Query::new();
        subset_query.insert_key(vec![1u8]);
        subset_query.insert_key(vec![3u8]);
        let subset_pq = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], subset_query);

        let (root_hash, results) = grovedb_proof
            .verify_subset(&subset_pq, grove_version)
            .expect("verify_subset should succeed");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 2, "subset should return 2 results");
    }

    #[test]
    fn grovedb_proof_verify_subset_with_absence_proof_method() {
        // Exercise GroveDBProof::verify_subset_with_absence_proof
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"x",
            Element::new_item(b"val_x".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert");

        // Generate a broad proof
        let mut full_query = Query::new();
        full_query.insert_all();
        let full_pq = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], full_query);

        let proof_bytes = db
            .prove_query(&full_pq, None, grove_version)
            .unwrap()
            .expect("should prove");

        let config = bincode::config::standard()
            .with_big_endian()
            .with_limit::<{ 256 * 1024 * 1024 }>();
        let (grovedb_proof, _): (GroveDBProof, _) =
            bincode::decode_from_slice(&proof_bytes, config).expect("should decode");

        // Verify subset with absence for a key that does not exist
        let mut subset_query = Query::new();
        subset_query.insert_key(b"x".to_vec());
        subset_query.insert_key(b"missing_key".to_vec());
        let subset_pq = PathQuery::new(
            vec![TEST_LEAF.to_vec()],
            SizedQuery::new(subset_query, Some(5), None),
        );

        let (root_hash, results) = grovedb_proof
            .verify_subset_with_absence_proof(&subset_pq, grove_version)
            .expect("verify_subset_with_absence_proof should succeed");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 2, "should have 2 results (present + absent)");
    }

    #[test]
    fn grovedb_proof_display_v0() {
        // Exercise Display for GroveDBProofV0 and GroveDBProof
        let grove_version = &GROVE_V2;
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"d1",
            Element::new_item(b"v1".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert");

        let mut query = Query::new();
        query.insert_key(b"d1".to_vec());
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove");

        let config = bincode::config::standard()
            .with_big_endian()
            .with_limit::<{ 256 * 1024 * 1024 }>();
        let (grovedb_proof, _): (GroveDBProof, _) =
            bincode::decode_from_slice(&proof_bytes, config).expect("should decode");

        let display = format!("{}", grovedb_proof);
        assert!(
            display.contains("GroveDBProofV0"),
            "display should mention V0: {}",
            display
        );
    }

    #[test]
    fn grovedb_proof_display_v1() {
        // Exercise Display for GroveDBProofV1 and the V1 path in GroveDBProof::Display
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"tree1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");

        db.insert(
            [b"tree1"].as_ref(),
            b"item1",
            Element::new_item(b"v1".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        let mut query = Query::new();
        query.insert_key(b"item1".to_vec());
        let path_query = PathQuery::new_unsized(vec![b"tree1".to_vec()], query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1");

        let config = bincode::config::standard()
            .with_big_endian()
            .with_limit::<{ 256 * 1024 * 1024 }>();
        let (grovedb_proof, _): (GroveDBProof, _) =
            bincode::decode_from_slice(&proof_bytes, config).expect("should decode");

        let display = format!("{}", grovedb_proof);
        assert!(
            display.contains("GroveDBProofV1"),
            "display should mention V1: {}",
            display
        );
    }

    // =========================================================================
    // 3. operations/proof/verify.rs coverage
    // =========================================================================

    #[test]
    fn verify_with_corrupt_proof_bytes() {
        // Feeding garbage bytes to verify_query should produce an error
        let grove_version = GroveVersion::latest();
        let garbage = vec![0xFF, 0xFE, 0xFD, 0xFC, 0xFB, 0xFA];

        let mut query = Query::new();
        query.insert_all();
        let path_query = PathQuery::new_unsized(vec![b"any".to_vec()], query);

        let result = GroveDb::verify_query(&garbage, &path_query, grove_version);
        assert!(
            result.is_err(),
            "corrupt proof bytes should fail verification"
        );
    }

    #[test]
    fn verify_query_with_offset_errors() {
        // Offsets are not supported for proof verification
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"item",
            Element::new_item(b"val".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert");

        let mut query = Query::new();
        query.insert_all();
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query.clone());

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove");

        // Now try to verify with an offset in the query
        let offset_pq = PathQuery::new(
            vec![TEST_LEAF.to_vec()],
            SizedQuery::new(query, None, Some(1)),
        );

        let result = GroveDb::verify_query_with_options(
            &proof_bytes,
            &offset_pq,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        );
        assert!(
            result.is_err(),
            "verification with offset should return error"
        );
    }

    #[test]
    fn verify_absence_proof_without_limit_errors() {
        // Absence proofs require a limit
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"x",
            Element::new_item(b"v".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert");

        let mut query = Query::new();
        query.insert_key(b"x".to_vec());
        query.insert_key(b"missing".to_vec());
        // No limit
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove");

        let result =
            GroveDb::verify_query_with_absence_proof(&proof_bytes, &path_query, grove_version);
        assert!(result.is_err(), "absence proof without limit should fail");
    }

    #[test]
    fn verify_subset_query_static_method() {
        // Exercise the static GroveDb::verify_subset_query method
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        for i in 0..5u8 {
            db.insert(
                [TEST_LEAF].as_ref(),
                &format!("key{}", i).into_bytes(),
                Element::new_item(format!("val{}", i).into_bytes()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert");
        }

        // Generate a full proof
        let mut full_query = Query::new();
        full_query.insert_all();
        let full_pq = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], full_query);

        let proof_bytes = db
            .prove_query(&full_pq, None, grove_version)
            .unwrap()
            .expect("should prove");

        // Verify with a subset query
        let mut subset_query = Query::new();
        subset_query.insert_key(b"key2".to_vec());
        let subset_pq = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], subset_query);

        let (root_hash, results) =
            GroveDb::verify_subset_query(&proof_bytes, &subset_pq, grove_version)
                .expect("verify_subset_query should succeed");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 1, "subset should return 1 result");
    }

    #[test]
    fn verify_subset_query_with_absence_proof_static() {
        // Exercise the static GroveDb::verify_subset_query_with_absence_proof
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"present",
            Element::new_item(b"val".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert");

        let mut full_query = Query::new();
        full_query.insert_all();
        let full_pq = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], full_query);

        let proof_bytes = db
            .prove_query(&full_pq, None, grove_version)
            .unwrap()
            .expect("should prove");

        let mut subset_query = Query::new();
        subset_query.insert_key(b"present".to_vec());
        subset_query.insert_key(b"absent".to_vec());
        let subset_pq = PathQuery::new(
            vec![TEST_LEAF.to_vec()],
            SizedQuery::new(subset_query, Some(5), None),
        );

        let (root_hash, results) = GroveDb::verify_subset_query_with_absence_proof(
            &proof_bytes,
            &subset_pq,
            grove_version,
        )
        .expect("verify_subset_query_with_absence_proof should succeed");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 2, "should have present + absent");
    }

    #[test]
    fn verify_subset_query_get_parent_tree_info_static() {
        // Exercise GroveDb::verify_subset_query_get_parent_tree_info
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"sum_tree",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum tree");

        db.insert(
            [TEST_LEAF, b"sum_tree"].as_ref(),
            b"s1",
            Element::new_sum_item(100),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum item");

        let mut query = Query::new();
        query.insert_all();
        let path_query =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"sum_tree".to_vec()], query);

        // Use full proof
        let mut broad_query = Query::new();
        broad_query.insert_all();
        let broad_pq =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"sum_tree".to_vec()], broad_query);

        let proof_bytes = db
            .prove_query(&broad_pq, None, grove_version)
            .unwrap()
            .expect("should prove");

        let (root_hash, feature_type, results) = GroveDb::verify_subset_query_get_parent_tree_info(
            &proof_bytes,
            &path_query,
            grove_version,
        )
        .expect("should verify subset parent tree info");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert!(
            matches!(
                feature_type,
                grovedb_merk::TreeFeatureType::SummedMerkNode(100)
            ),
            "parent should be SummedMerkNode(100), got {:?}",
            feature_type
        );
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn verify_parent_tree_info_with_subquery_errors() {
        // verify_query_get_parent_tree_info_with_options errors when query has subquery
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"sub",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert");

        db.insert(
            [TEST_LEAF, b"sub"].as_ref(),
            b"item",
            Element::new_item(b"v".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert");

        // Query with a subquery
        let mut inner_query = Query::new();
        inner_query.insert_all();
        let mut query = Query::new();
        query.insert_all();
        query.set_subquery(inner_query);
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove");

        let result =
            GroveDb::verify_query_get_parent_tree_info(&proof_bytes, &path_query, grove_version);
        assert!(
            result.is_err(),
            "parent tree info is not available with subqueries"
        );
    }

    #[test]
    fn verify_query_with_chained_path_queries() {
        // Exercise verify_query_with_chained_path_queries
        let grove_version = GroveVersion::latest();
        let db = make_deep_tree(grove_version);

        // First query: get all items under [TEST_LEAF, "innertree"]
        let mut first_query = Query::new();
        first_query.insert_all();
        let first_pq =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree".to_vec()], first_query);

        // Generate a broad proof that covers multiple subtrees
        let mut broad_outer = Query::new();
        broad_outer.insert_all();
        let mut broad_inner = Query::new();
        broad_inner.insert_all();
        broad_outer.set_subquery(broad_inner);
        let broad_pq = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], broad_outer);

        let proof_bytes = db
            .prove_query(&broad_pq, None, grove_version)
            .unwrap()
            .expect("should prove broad query");

        // Chain: after getting results from innertree, query innertree4
        let chained: Vec<Box<dyn Fn(Vec<_>) -> Option<PathQuery>>> = vec![Box::new(|_results| {
            let mut q = Query::new();
            q.insert_all();
            Some(PathQuery::new_unsized(
                vec![TEST_LEAF.to_vec(), b"innertree4".to_vec()],
                q,
            ))
        })];

        let (root_hash, all_results) = GroveDb::verify_query_with_chained_path_queries(
            &proof_bytes,
            &first_pq,
            chained,
            grove_version,
        )
        .expect("chained queries should verify");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(
            all_results.len(),
            2,
            "should have 2 result sets (first + chained)"
        );
        // innertree has 3 items (key1, key2, key3)
        assert_eq!(all_results[0].len(), 3, "innertree should have 3 items");
        // innertree4 has 2 items (key4, key5)
        assert_eq!(all_results[1].len(), 2, "innertree4 should have 2 items");
    }

    #[test]
    fn verify_chained_query_generator_returns_none_errors() {
        // If a chained path query generator returns None, it should error
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"item",
            Element::new_item(b"v".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert");

        let mut query = Query::new();
        query.insert_all();
        let first_pq = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        let proof_bytes = db
            .prove_query(&first_pq, None, grove_version)
            .unwrap()
            .expect("should prove");

        let chained: Vec<Box<dyn Fn(Vec<_>) -> Option<PathQuery>>> =
            vec![Box::new(|_results| None)];

        let result = GroveDb::verify_query_with_chained_path_queries(
            &proof_bytes,
            &first_pq,
            chained,
            grove_version,
        );
        assert!(result.is_err(), "chained query returning None should error");
    }

    #[test]
    fn verify_v1_proof_with_merk_subtree() {
        // V1 proof with standard Merk subtree (not MMR/BulkAppend)
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert a subtree with items
        db.insert(
            [TEST_LEAF].as_ref(),
            b"sub",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree");

        for i in 0..3u8 {
            db.insert(
                [TEST_LEAF, b"sub"].as_ref(),
                &[i],
                Element::new_item(vec![i + 100]),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert item");
        }

        // Build a query with subquery to descend into "sub"
        let mut inner_query = Query::new();
        inner_query.insert_all();
        let mut outer_query = Query::new();
        outer_query.insert_key(b"sub".to_vec());
        outer_query.set_subquery(inner_query);
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], outer_query);

        // Generate V1 proof
        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should generate V1 proof");

        // Verify with the standard verify_query_with_options (handles both V0 and V1)
        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify V1 proof with merk subtree");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 3, "should have 3 items from subtree");
    }

    #[test]
    fn verify_v1_proof_with_add_parent_tree_on_subquery() {
        // Exercise the add_parent_tree_on_subquery v2 query feature
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert a SumTree with items
        db.insert(
            [TEST_LEAF].as_ref(),
            b"st",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum tree");

        db.insert(
            [TEST_LEAF, b"st"].as_ref(),
            b"a",
            Element::new_sum_item(10),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum item");

        db.insert(
            [TEST_LEAF, b"st"].as_ref(),
            b"b",
            Element::new_sum_item(20),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum item");

        // Build a query with add_parent_tree_on_subquery = true
        let mut inner = Query::new();
        inner.insert_all();

        let query = Query {
            items: vec![QueryItem::Key(b"st".to_vec())],
            default_subquery_branch: SubqueryBranch {
                subquery_path: None,
                subquery: Some(Box::new(inner)),
            },
            left_to_right: true,
            conditional_subquery_branches: None,
            add_parent_tree_on_subquery: true,
        };
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        // V1 proof
        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should generate V1 proof with add_parent_tree_on_subquery");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        // With add_parent_tree_on_subquery, the parent (SumTree) is included in
        // results alongside the items
        assert!(
            results.len() >= 2,
            "should have at least the 2 sum items, got {}",
            results.len()
        );
    }

    #[test]
    fn verify_v1_absence_proof() {
        // V1 proof with absence proofs
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");

        db.insert(
            [b"tree"].as_ref(),
            b"exists",
            Element::new_item(b"val".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        let mut query = Query::new();
        query.insert_key(b"exists".to_vec());
        query.insert_key(b"ghost".to_vec());
        let path_query = PathQuery::new(
            vec![b"tree".to_vec()],
            SizedQuery::new(query, Some(5), None),
        );

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: true,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify v1 absence proof");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 2, "should have 2 entries (present + absent)");

        // "exists" should be present, "ghost" should be None
        let exists_entry = results.iter().find(|(_, k, _)| k == b"exists");
        assert!(
            exists_entry.is_some(),
            "should find 'exists' key in results"
        );
        assert!(
            exists_entry.unwrap().2.is_some(),
            "'exists' should have a value"
        );

        let ghost_entry = results.iter().find(|(_, k, _)| k == b"ghost");
        assert!(ghost_entry.is_some(), "should find 'ghost' key in results");
        assert!(
            ghost_entry.unwrap().2.is_none(),
            "'ghost' should be absent (None)"
        );
    }

    #[test]
    fn verify_v1_raw_proof() {
        // V1 proof verified via verify_query_raw
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"t",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");

        db.insert(
            [b"t"].as_ref(),
            b"k",
            Element::new_item(b"v".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        let mut query = Query::new();
        query.insert_all();
        let path_query = PathQuery::new_unsized(vec![b"t".to_vec()], query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1");

        let (root_hash, raw_results) =
            GroveDb::verify_query_raw(&proof_bytes, &path_query, grove_version)
                .expect("verify_query_raw on v1 proof should succeed");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert!(!raw_results.is_empty(), "should have raw results");
    }

    // =========================================================================
    // 4. operations/proof/generate.rs coverage
    // =========================================================================

    #[test]
    fn prove_query_with_limit_zero_errors() {
        // prove_query with limit 0 should return InvalidQuery error
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let mut query = Query::new();
        query.insert_all();
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec()],
            SizedQuery::new(query, Some(0), None),
        );

        let result = db.prove_query(&path_query, None, grove_version).unwrap();
        assert!(result.is_err(), "prove_query with limit 0 should error");
    }

    #[test]
    fn prove_query_with_nonzero_offset_errors() {
        // prove_query with a non-zero offset should return InvalidQuery error
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let mut query = Query::new();
        query.insert_all();
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec()],
            SizedQuery::new(query, None, Some(5)),
        );

        let result = db.prove_query(&path_query, None, grove_version).unwrap();
        assert!(
            result.is_err(),
            "prove_query with non-zero offset should error"
        );
    }

    #[test]
    fn prove_query_v1_with_limit_zero_errors() {
        // prove_query_v1 with limit 0 should also error
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let mut query = Query::new();
        query.insert_all();
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec()],
            SizedQuery::new(query, Some(0), None),
        );

        let result = db.prove_query(&path_query, None, grove_version).unwrap();
        assert!(result.is_err(), "prove_query_v1 with limit 0 should error");
    }

    #[test]
    fn prove_query_v1_with_nonzero_offset_errors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let mut query = Query::new();
        query.insert_all();
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec()],
            SizedQuery::new(query, None, Some(3)),
        );

        let result = db.prove_query(&path_query, None, grove_version).unwrap();
        assert!(
            result.is_err(),
            "prove_query_v1 with non-zero offset should error"
        );
    }

    #[test]
    fn prove_v0_on_mmr_tree_errors() {
        // V0 proofs should error when encountering MmrTree with subquery
        let grove_version = &GROVE_V2;
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"mmr",
            Element::empty_mmr_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert mmr tree");

        // Append some data
        db.mmr_tree_append(EMPTY_PATH, b"mmr", b"leaf0".to_vec(), None, grove_version)
            .unwrap()
            .expect("should append");

        // Build a query with subquery into the MmrTree
        let mut inner_query = Query::new();
        inner_query.insert_key(0u64.to_be_bytes().to_vec());
        let query = Query {
            items: vec![QueryItem::Key(b"mmr".to_vec())],
            default_subquery_branch: SubqueryBranch {
                subquery_path: None,
                subquery: Some(Box::new(inner_query)),
            },
            left_to_right: true,
            conditional_subquery_branches: None,
            add_parent_tree_on_subquery: false,
        };
        let path_query = PathQuery::new_unsized(vec![], query);

        // V0 prove should error
        let result = db.prove_query(&path_query, None, grove_version).unwrap();
        assert!(
            result.is_err(),
            "V0 proofs should not support MmrTree subqueries"
        );
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("V0 proofs do not support"),
            "error should mention V0 limitation: {}",
            err_msg
        );
    }

    #[test]
    fn prove_and_verify_with_empty_subtree() {
        // Prove a query on an empty subtree (exercises limit decrease on empty result)
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert an empty subtree
        db.insert(
            [TEST_LEAF].as_ref(),
            b"empty_sub",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert empty subtree");

        // Query with subquery into the empty subtree
        let mut inner = Query::new();
        inner.insert_all();
        let mut outer = Query::new();
        outer.insert_key(b"empty_sub".to_vec());
        outer.set_subquery(inner);
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec()],
            SizedQuery::new(outer, Some(5), None),
        );

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove query on empty subtree");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify proof for empty subtree");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        // Empty subtree should produce 0 results
        assert_eq!(results.len(), 0, "empty subtree should have 0 results");
    }

    #[test]
    fn prove_and_verify_with_include_empty_trees() {
        // Exercise include_empty_trees_in_result option
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert items alongside empty trees
        db.insert(
            [TEST_LEAF].as_ref(),
            b"item_a",
            Element::new_item(b"va".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        db.insert(
            [TEST_LEAF].as_ref(),
            b"empty_tree",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert empty tree");

        let mut query = Query::new();
        query.insert_all();
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove");

        // Verify with include_empty_trees = true
        let (_, results_with_empty) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: true,
            },
            grove_version,
        )
        .expect("should verify with include empty trees");

        // Verify without include_empty_trees
        let (_, results_without_empty) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify without include empty trees");

        // With empty trees included, we should have more results
        assert!(
            results_with_empty.len() >= results_without_empty.len(),
            "including empty trees should yield >= results: {} vs {}",
            results_with_empty.len(),
            results_without_empty.len()
        );
    }

    #[test]
    fn prove_query_many_with_custom_prove_options() {
        // Exercise prove_query_many with explicit prove options
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"pm",
            Element::new_item(b"pv".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert");

        let mut query = Query::new();
        query.insert_all();
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        let prove_options = ProveOptions {
            decrease_limit_on_empty_sub_query_result: false,
        };

        let proof_bytes = db
            .prove_query_many(vec![&path_query], Some(prove_options), grove_version)
            .unwrap()
            .expect("prove_query_many with custom options should succeed");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert!(!results.is_empty());
    }

    #[test]
    fn prove_and_verify_with_limit_matching_result_count() {
        // When limit exactly matches the number of results, the proof should
        // still verify correctly
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        for i in 0..5u8 {
            db.insert(
                [TEST_LEAF].as_ref(),
                &format!("e_{}", i).into_bytes(),
                Element::new_item(format!("v_{}", i).into_bytes()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert");
        }

        let mut query = Query::new();
        query.insert_all();
        // Limit = 5 exactly matches the 5 items
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec()],
            SizedQuery::new(query, Some(5), None),
        );

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove with exact limit");

        let (root_hash, results) = GroveDb::verify_query(&proof_bytes, &path_query, grove_version)
            .expect("should verify with exact limit");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 5, "should get exactly 5 results");
    }

    #[test]
    fn prove_and_verify_spanning_multiple_subtrees() {
        // Prove a query that spans multiple subtrees (multi-level proof)
        let grove_version = GroveVersion::latest();
        let db = make_deep_tree(grove_version);

        // Query that descends into deep_leaf -> deep_node_1 -> deeper_1 and deeper_2
        // Path: [deep_leaf, deep_node_1], query all, subquery all
        let mut inner = Query::new();
        inner.insert_all();
        let mut outer = Query::new();
        outer.insert_all();
        outer.set_subquery(inner);
        let path_query =
            PathQuery::new_unsized(vec![b"deep_leaf".to_vec(), b"deep_node_1".to_vec()], outer);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove multi-subtree query");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify multi-subtree proof");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        // deeper_1 has k1,k2,k3 and deeper_2 has k4,k5,k6 = 6 total
        assert_eq!(
            results.len(),
            6,
            "should have 6 items from 2 deeper subtrees"
        );
    }

    #[test]
    fn prove_v1_spanning_multiple_subtrees() {
        // V1 proof spanning multiple Merk subtrees
        let grove_version = GroveVersion::latest();
        let db = make_deep_tree(grove_version);

        let mut inner = Query::new();
        inner.insert_all();
        let mut outer = Query::new();
        outer.insert_all();
        outer.set_subquery(inner);
        let path_query =
            PathQuery::new_unsized(vec![b"deep_leaf".to_vec(), b"deep_node_2".to_vec()], outer);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1 multi-subtree");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify v1 multi-subtree proof");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        // deep_node_2 has deeper_3 (k7,k8,k9), deeper_4 (k10,k11), deeper_5 (k12,k13,k14)
        assert_eq!(
            results.len(),
            8,
            "should have 8 items from 3 deeper subtrees"
        );
    }

    #[test]
    fn prove_and_verify_right_to_left() {
        // Exercise right-to-left query direction in proof generation/verification
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        for i in 0..5u8 {
            db.insert(
                [TEST_LEAF].as_ref(),
                &format!("rtl_{}", i).into_bytes(),
                Element::new_item(format!("rv_{}", i).into_bytes()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert");
        }

        let query = Query {
            items: vec![QueryItem::RangeFull(std::ops::RangeFull)],
            left_to_right: false, // right to left
            default_subquery_branch: SubqueryBranch::default(),
            conditional_subquery_branches: None,
            add_parent_tree_on_subquery: false,
        };
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec()],
            SizedQuery::new(query, Some(3), None),
        );

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove right-to-left query");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify right-to-left proof");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 3, "should have 3 results");

        // Right-to-left: should get the last 3 items in reverse order
        assert_eq!(results[0].1, b"rtl_4".to_vec());
        assert_eq!(results[1].1, b"rtl_3".to_vec());
        assert_eq!(results[2].1, b"rtl_2".to_vec());
    }

    #[test]
    fn prove_v1_right_to_left() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");

        for i in 0..4u8 {
            db.insert(
                [b"tree"].as_ref(),
                &[b'a' + i],
                Element::new_item(vec![i]),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert item");
        }

        let query = Query {
            items: vec![QueryItem::RangeFull(std::ops::RangeFull)],
            left_to_right: false,
            default_subquery_branch: SubqueryBranch::default(),
            conditional_subquery_branches: None,
            add_parent_tree_on_subquery: false,
        };
        let path_query = PathQuery::new(
            vec![b"tree".to_vec()],
            SizedQuery::new(query, Some(2), None),
        );

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1 rtl");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify v1 rtl");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 2, "should have 2 results");
        // Right-to-left: d, c
        assert_eq!(results[0].1, vec![b'd']);
        assert_eq!(results[1].1, vec![b'c']);
    }

    #[test]
    fn prove_and_verify_non_serialized() {
        // Exercise prove_query_non_serialized directly
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"ns1",
            Element::new_item(b"nv1".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert");

        let mut query = Query::new();
        query.insert_key(b"ns1".to_vec());
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        let grovedb_proof = db
            .prove_query_non_serialized(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove non-serialized");

        // Verify directly on the proof struct
        let (root_hash, results) = grovedb_proof
            .verify(&path_query, grove_version)
            .expect("should verify non-serialized proof");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn prove_v1_non_serialized() {
        // Exercise prove_query_non_serialized_v1 directly
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"t",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");

        db.insert(
            [b"t"].as_ref(),
            b"k",
            Element::new_item(b"v".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        let mut query = Query::new();
        query.insert_all();
        let path_query = PathQuery::new_unsized(vec![b"t".to_vec()], query);

        let grovedb_proof = db
            .prove_query_non_serialized_v1(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1 non-serialized");

        // Should be V1 variant
        assert!(
            matches!(grovedb_proof, GroveDBProof::V1(_)),
            "non-serialized v1 proof should be V1 variant"
        );

        let (root_hash, results) = grovedb_proof
            .verify(&path_query, grove_version)
            .expect("should verify v1 non-serialized proof");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn prove_decrease_limit_on_empty_false() {
        // Exercise prove with decrease_limit_on_empty_sub_query_result = false
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Create empty subtrees
        db.insert(
            [TEST_LEAF].as_ref(),
            b"empty_a",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert");

        db.insert(
            [TEST_LEAF].as_ref(),
            b"empty_b",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert");

        let mut inner = Query::new();
        inner.insert_all();
        let mut outer = Query::new();
        outer.insert_all();
        outer.set_subquery(inner);
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec()],
            SizedQuery::new(outer, Some(10), None),
        );

        let options = ProveOptions {
            decrease_limit_on_empty_sub_query_result: false,
        };

        let proof_bytes = db
            .prove_query(&path_query, Some(options), grove_version)
            .unwrap()
            .expect("should prove with decrease_limit=false");

        // Verification should succeed
        let (root_hash, _results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify proof with decrease_limit=false");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
    }

    #[test]
    fn prove_v1_decrease_limit_on_empty_false() {
        // Same as above but for V1 proofs
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"empty_v1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert");

        let mut inner = Query::new();
        inner.insert_all();
        let mut outer = Query::new();
        outer.insert_key(b"empty_v1".to_vec());
        outer.set_subquery(inner);
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec()],
            SizedQuery::new(outer, Some(5), None),
        );

        let options = ProveOptions {
            decrease_limit_on_empty_sub_query_result: false,
        };

        let proof_bytes = db
            .prove_query(&path_query, Some(options), grove_version)
            .unwrap()
            .expect("should prove v1 with decrease_limit=false");

        let (root_hash, _results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify v1 with decrease_limit=false");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
    }

    #[test]
    fn prove_v1_with_sum_tree_no_subquery() {
        // V1 proof where SumTree is a leaf (no subquery) - tree itself is returned
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"sum",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum tree");

        db.insert(
            [TEST_LEAF, b"sum"].as_ref(),
            b"a",
            Element::new_sum_item(42),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum item");

        // Query just the sum tree key without subquery
        let mut query = Query::new();
        query.insert_key(b"sum".to_vec());
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1 for sum tree leaf");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: true,
            },
            grove_version,
        )
        .expect("should verify");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(
            results.len(),
            1,
            "should return the sum tree element itself"
        );
    }

    #[test]
    fn prove_v1_with_mixed_element_types() {
        // V1 proof with a mix of Items, SumItems, Trees, and empty trees
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Regular item
        db.insert(
            [TEST_LEAF].as_ref(),
            b"item",
            Element::new_item(b"iv".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        // Empty tree
        db.insert(
            [TEST_LEAF].as_ref(),
            b"empty",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert empty tree");

        // Non-empty tree
        db.insert(
            [TEST_LEAF].as_ref(),
            b"full",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");

        db.insert(
            [TEST_LEAF, b"full"].as_ref(),
            b"child",
            Element::new_item(b"cv".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert child");

        let mut query = Query::new();
        query.insert_all();
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1 mixed types");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: true,
            },
            grove_version,
        )
        .expect("should verify v1 mixed types");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        // Should include: item + empty tree (with include_empty) + full tree
        assert!(
            results.len() >= 2,
            "should have multiple results: {}",
            results.len()
        );
    }

    #[test]
    fn prove_and_verify_reference_resolution() {
        // Exercise the reference resolution path in proof generation
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert an item
        db.insert(
            [TEST_LEAF].as_ref(),
            b"target",
            Element::new_item(b"target_val".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert target");

        // Insert a reference to the item
        db.insert(
            [TEST_LEAF].as_ref(),
            b"ref",
            Element::new_reference(crate::reference_path::ReferencePathType::SiblingReference(
                b"target".to_vec(),
            )),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert reference");

        let mut query = Query::new();
        query.insert_key(b"ref".to_vec());
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove query with reference");

        let (root_hash, results) = GroveDb::verify_query(&proof_bytes, &path_query, grove_version)
            .expect("should verify proof with reference");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(
            results.len(),
            1,
            "should return 1 result (resolved reference)"
        );
        // The resolved reference should return the target's item
        let element = results[0].2.as_ref().expect("element should exist");
        match element {
            Element::Item(data, _) => {
                assert_eq!(
                    data,
                    &b"target_val".to_vec(),
                    "should resolve to target value"
                );
            }
            _ => panic!(
                "expected Item after reference resolution, got {:?}",
                element
            ),
        }
    }

    #[test]
    fn prove_v1_reference_resolution() {
        // V1 proof with reference resolution
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"actual",
            Element::new_item(b"real_data".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert");

        db.insert(
            [TEST_LEAF].as_ref(),
            b"pointer",
            Element::new_reference(crate::reference_path::ReferencePathType::SiblingReference(
                b"actual".to_vec(),
            )),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert reference");

        let mut query = Query::new();
        query.insert_key(b"pointer".to_vec());
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1 with reference");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify v1 with reference");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 1);
        let elem = results[0].2.as_ref().expect("should have element");
        match elem {
            Element::Item(data, _) => {
                assert_eq!(data, &b"real_data".to_vec());
            }
            _ => panic!("expected resolved Item, got {:?}", elem),
        }
    }

    // =========================================================================
    // 5. Additional verify.rs coverage — SumTree / CountTree / BigSumTree /
    //    CountSumTree verification paths, conditional subqueries, v2 paths
    // =========================================================================

    #[test]
    fn prove_v1_sum_tree_with_subquery() {
        // V1 proof where SumTree is queried WITH a subquery (descends into children)
        // Exercises the verify_layer_proof_v1 SumTree(Some(_)) match arm
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"root",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert root tree");

        db.insert(
            [b"root"].as_ref(),
            b"sum",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum tree");

        db.insert(
            [b"root".as_slice(), b"sum".as_slice()].as_ref(),
            b"s1",
            Element::new_sum_item(10),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum item");

        db.insert(
            [b"root".as_slice(), b"sum".as_slice()].as_ref(),
            b"s2",
            Element::new_sum_item(20),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum item");

        db.insert(
            [b"root".as_slice(), b"sum".as_slice()].as_ref(),
            b"s3",
            Element::new_sum_item(30),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum item");

        // Query SumTree with subquery to get its children
        let mut inner = Query::new();
        inner.insert_all();
        let mut outer = Query::new();
        outer.insert_key(b"sum".to_vec());
        outer.set_subquery(inner);
        let path_query = PathQuery::new_unsized(vec![b"root".to_vec()], outer);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1 sum tree with subquery");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify v1 sum tree with subquery");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 3, "should have 3 sum items");
    }

    #[test]
    fn prove_v1_count_tree_with_subquery() {
        // V1 proof with CountTree containing items
        // Exercises the CountTree(Some(_)) match arm in verify_layer_proof_v1
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"root",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert root");

        db.insert(
            [b"root"].as_ref(),
            b"ct",
            Element::empty_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert count tree");

        db.insert(
            [b"root".as_slice(), b"ct".as_slice()].as_ref(),
            b"c1",
            Element::new_item(b"cv1".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item in count tree");

        db.insert(
            [b"root".as_slice(), b"ct".as_slice()].as_ref(),
            b"c2",
            Element::new_item(b"cv2".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item in count tree");

        let mut inner = Query::new();
        inner.insert_all();
        let mut outer = Query::new();
        outer.insert_key(b"ct".to_vec());
        outer.set_subquery(inner);
        let path_query = PathQuery::new_unsized(vec![b"root".to_vec()], outer);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1 count tree with subquery");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify v1 count tree with subquery");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 2, "should have 2 items from count tree");
    }

    #[test]
    fn prove_v1_count_sum_tree_with_subquery() {
        // V1 proof with CountSumTree containing sum items
        // Exercises the CountSumTree(Some(_)) match arm
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"root",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert root");

        db.insert(
            [b"root"].as_ref(),
            b"cst",
            Element::empty_count_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert count sum tree");

        db.insert(
            [b"root".as_slice(), b"cst".as_slice()].as_ref(),
            b"x",
            Element::new_sum_item(100),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum item in count sum tree");

        db.insert(
            [b"root".as_slice(), b"cst".as_slice()].as_ref(),
            b"y",
            Element::new_sum_item(200),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum item in count sum tree");

        let mut inner = Query::new();
        inner.insert_all();
        let mut outer = Query::new();
        outer.insert_key(b"cst".to_vec());
        outer.set_subquery(inner);
        let path_query = PathQuery::new_unsized(vec![b"root".to_vec()], outer);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1 count sum tree with subquery");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify v1 count sum tree with subquery");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(
            results.len(),
            2,
            "should have 2 sum items from count sum tree"
        );
    }

    #[test]
    fn prove_v1_big_sum_tree_with_subquery() {
        // V1 proof with BigSumTree containing sum items
        // Exercises the BigSumTree(Some(_)) match arm
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"root",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert root");

        db.insert(
            [b"root"].as_ref(),
            b"bst",
            Element::empty_big_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert big sum tree");

        db.insert(
            [b"root".as_slice(), b"bst".as_slice()].as_ref(),
            b"b1",
            Element::new_sum_item(500),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum item in big sum tree");

        let mut inner = Query::new();
        inner.insert_all();
        let mut outer = Query::new();
        outer.insert_key(b"bst".to_vec());
        outer.set_subquery(inner);
        let path_query = PathQuery::new_unsized(vec![b"root".to_vec()], outer);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1 big sum tree with subquery");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify v1 big sum tree with subquery");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 1, "should have 1 sum item from big sum tree");
    }

    #[test]
    fn prove_v1_add_parent_tree_on_count_tree() {
        // V2 query with add_parent_tree_on_subquery on a CountTree
        // Exercises the should_add_parent_tree_at_path path in verify_layer_proof_v1
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"root",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert root");

        db.insert(
            [b"root"].as_ref(),
            b"ct",
            Element::empty_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert count tree");

        db.insert(
            [b"root".as_slice(), b"ct".as_slice()].as_ref(),
            b"item1",
            Element::new_item(b"val1".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        db.insert(
            [b"root".as_slice(), b"ct".as_slice()].as_ref(),
            b"item2",
            Element::new_item(b"val2".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        // Build a query with add_parent_tree_on_subquery = true
        let mut inner = Query::new();
        inner.insert_all();
        let query = Query {
            items: vec![QueryItem::Key(b"ct".to_vec())],
            default_subquery_branch: SubqueryBranch {
                subquery_path: None,
                subquery: Some(Box::new(inner)),
            },
            left_to_right: true,
            conditional_subquery_branches: None,
            add_parent_tree_on_subquery: true,
        };
        let path_query = PathQuery::new_unsized(vec![b"root".to_vec()], query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1 with add_parent_tree on count tree");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify v1 with add_parent_tree on count tree");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        // Should include the parent tree + the 2 child items
        assert!(
            results.len() >= 2,
            "should have at least 2 results (items + possibly parent), got {}",
            results.len()
        );
    }

    #[test]
    fn prove_v1_add_parent_tree_on_big_sum_tree() {
        // V2 query with add_parent_tree_on_subquery on a BigSumTree
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"root",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert root");

        db.insert(
            [b"root"].as_ref(),
            b"bst",
            Element::empty_big_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert big sum tree");

        db.insert(
            [b"root".as_slice(), b"bst".as_slice()].as_ref(),
            b"val",
            Element::new_sum_item(999),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum item");

        let mut inner = Query::new();
        inner.insert_all();
        let query = Query {
            items: vec![QueryItem::Key(b"bst".to_vec())],
            default_subquery_branch: SubqueryBranch {
                subquery_path: None,
                subquery: Some(Box::new(inner)),
            },
            left_to_right: true,
            conditional_subquery_branches: None,
            add_parent_tree_on_subquery: true,
        };
        let path_query = PathQuery::new_unsized(vec![b"root".to_vec()], query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1 with add_parent_tree on big sum tree");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify v1 add_parent_tree on big sum tree");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert!(
            !results.is_empty(),
            "should have results with add_parent_tree on big sum tree"
        );
    }

    #[test]
    fn prove_v1_mmr_tree_with_subquery() {
        // V1 proof with MmrTree queried via subquery
        // Exercises the MMR proof generation and verify_mmr_lower_layer paths
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"root",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert root");

        db.insert(
            [b"root"].as_ref(),
            b"mmr",
            Element::empty_mmr_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert mmr tree");

        // Append several leaves
        for i in 0..4u8 {
            db.mmr_tree_append(
                [b"root"].as_ref(),
                b"mmr",
                vec![i + 100],
                None,
                grove_version,
            )
            .unwrap()
            .expect("should append to mmr");
        }

        // Build query: root -> mmr -> (subquery for leaf indices 0 and 2)
        let mut inner = Query::new();
        inner.insert_key(0u64.to_be_bytes().to_vec());
        inner.insert_key(2u64.to_be_bytes().to_vec());
        let query = Query {
            items: vec![QueryItem::Key(b"mmr".to_vec())],
            default_subquery_branch: SubqueryBranch {
                subquery_path: None,
                subquery: Some(Box::new(inner)),
            },
            left_to_right: true,
            conditional_subquery_branches: None,
            add_parent_tree_on_subquery: false,
        };
        let path_query = PathQuery::new_unsized(vec![b"root".to_vec()], query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1 mmr tree with subquery");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify v1 mmr tree proof");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 2, "should have 2 MMR leaf results");
    }

    #[test]
    fn prove_v1_bulk_append_tree_with_subquery() {
        // V1 proof with BulkAppendTree queried via subquery
        // Exercises the bulk append proof generation and verify_bulk_append_lower_layer
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"root",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert root");

        db.insert(
            [b"root"].as_ref(),
            b"bat",
            Element::empty_bulk_append_tree(2).expect("valid chunk_power"), // chunk_power = 2 (epoch_size = 4)
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert bulk append tree");

        // Append several values
        for i in 0..6u8 {
            db.bulk_append(
                [b"root"].as_ref(),
                b"bat",
                vec![i + 50],
                None,
                grove_version,
            )
            .unwrap()
            .expect("should append to bulk append tree");
        }

        // Build query: root -> bat -> (subquery for positions 0..3)
        let mut inner = Query::new();
        inner.insert_key(0u64.to_be_bytes().to_vec());
        inner.insert_key(1u64.to_be_bytes().to_vec());
        inner.insert_key(2u64.to_be_bytes().to_vec());
        let query = Query {
            items: vec![QueryItem::Key(b"bat".to_vec())],
            default_subquery_branch: SubqueryBranch {
                subquery_path: None,
                subquery: Some(Box::new(inner)),
            },
            left_to_right: true,
            conditional_subquery_branches: None,
            add_parent_tree_on_subquery: false,
        };
        let path_query = PathQuery::new_unsized(vec![b"root".to_vec()], query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1 bulk append tree with subquery");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify v1 bulk append tree proof");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 3, "should have 3 bulk append tree results");
    }

    #[test]
    fn prove_v1_dense_tree_with_subquery() {
        // V1 proof with DenseAppendOnlyFixedSizeTree queried via subquery
        // Exercises dense tree proof generation and verify_dense_tree_lower_layer
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"root",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert root");

        db.insert(
            [b"root"].as_ref(),
            b"dense",
            Element::empty_dense_tree(3), // height = 3, max 8 entries
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert dense tree");

        // Insert several values
        for i in 0..5u16 {
            db.dense_tree_insert(
                [b"root"].as_ref(),
                b"dense",
                vec![i as u8 + 10],
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert into dense tree");
        }

        // Build query: root -> dense -> (subquery for positions 1 and 3)
        let mut inner = Query::new();
        inner.insert_key(1u16.to_be_bytes().to_vec());
        inner.insert_key(3u16.to_be_bytes().to_vec());
        let query = Query {
            items: vec![QueryItem::Key(b"dense".to_vec())],
            default_subquery_branch: SubqueryBranch {
                subquery_path: None,
                subquery: Some(Box::new(inner)),
            },
            left_to_right: true,
            conditional_subquery_branches: None,
            add_parent_tree_on_subquery: false,
        };
        let path_query = PathQuery::new_unsized(vec![b"root".to_vec()], query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1 dense tree with subquery");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify v1 dense tree proof");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 2, "should have 2 dense tree results");
    }

    #[test]
    fn prove_v0_conditional_subquery_branches() {
        // V0 proof with conditional subquery branches
        // Different keys trigger different subqueries
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"root",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert root");

        // Two subtrees with different content
        db.insert(
            [b"root"].as_ref(),
            b"alpha",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert alpha tree");

        db.insert(
            [b"root".as_slice(), b"alpha".as_slice()].as_ref(),
            b"a1",
            Element::new_item(b"alpha_val1".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item in alpha");

        db.insert(
            [b"root"].as_ref(),
            b"beta",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert beta tree");

        db.insert(
            [b"root", b"beta"].as_ref(),
            b"b1",
            Element::new_item(b"beta_val1".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item in beta");

        db.insert(
            [b"root", b"beta"].as_ref(),
            b"b2",
            Element::new_item(b"beta_val2".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item in beta");

        // Build query with conditional subquery branches:
        // - alpha key -> subquery for key "a1"
        // - default branch -> subquery for all
        let mut alpha_subquery = Query::new();
        alpha_subquery.insert_key(b"a1".to_vec());

        let mut default_subquery = Query::new();
        default_subquery.insert_all();

        let mut conditional = IndexMap::new();
        conditional.insert(
            QueryItem::Key(b"alpha".to_vec()),
            SubqueryBranch {
                subquery_path: None,
                subquery: Some(Box::new(alpha_subquery)),
            },
        );

        let query = Query {
            items: vec![QueryItem::RangeFull(std::ops::RangeFull)],
            default_subquery_branch: SubqueryBranch {
                subquery_path: None,
                subquery: Some(Box::new(default_subquery)),
            },
            left_to_right: true,
            conditional_subquery_branches: Some(conditional),
            add_parent_tree_on_subquery: false,
        };
        let path_query = PathQuery::new_unsized(vec![b"root".to_vec()], query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v0 with conditional subqueries");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify v0 conditional subquery proof");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        // alpha: 1 item (a1), beta: 2 items (b1, b2) = 3 total
        assert_eq!(
            results.len(),
            3,
            "should have 3 results (1 from alpha + 2 from beta)"
        );
    }

    #[test]
    fn prove_v1_conditional_subquery_branches() {
        // V1 proof with conditional subquery branches
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"root",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert root");

        db.insert(
            [b"root"].as_ref(),
            b"treeA",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert treeA");

        db.insert(
            [b"root".as_slice(), b"treeA".as_slice()].as_ref(),
            b"a",
            Element::new_item(b"va".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert");

        db.insert(
            [b"root"].as_ref(),
            b"treeB",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert treeB");

        db.insert(
            [b"root".as_slice(), b"treeB".as_slice()].as_ref(),
            b"b",
            Element::new_item(b"vb".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert");

        // Conditional: treeA -> only key "a", treeB -> all
        let mut a_subq = Query::new();
        a_subq.insert_key(b"a".to_vec());
        let mut all_subq = Query::new();
        all_subq.insert_all();

        let mut conditional = IndexMap::new();
        conditional.insert(
            QueryItem::Key(b"treeA".to_vec()),
            SubqueryBranch {
                subquery_path: None,
                subquery: Some(Box::new(a_subq)),
            },
        );

        let query = Query {
            items: vec![QueryItem::RangeFull(std::ops::RangeFull)],
            default_subquery_branch: SubqueryBranch {
                subquery_path: None,
                subquery: Some(Box::new(all_subq)),
            },
            left_to_right: true,
            conditional_subquery_branches: Some(conditional),
            add_parent_tree_on_subquery: false,
        };
        let path_query = PathQuery::new_unsized(vec![b"root".to_vec()], query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1 conditional subqueries");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify v1 conditional subquery proof");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        // treeA: 1 item, treeB: 1 item = 2 total
        assert_eq!(results.len(), 2, "should have 2 results from conditional");
    }

    #[test]
    fn prove_v1_subquery_path() {
        // V1 proof using subquery_path (not just subquery) to navigate deeper
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"root",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert root");

        db.insert(
            [b"root"].as_ref(),
            b"level1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert level1");

        db.insert(
            [b"root".as_slice(), b"level1".as_slice()].as_ref(),
            b"level2",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert level2");

        db.insert(
            [
                b"root".as_slice(),
                b"level1".as_slice(),
                b"level2".as_slice(),
            ]
            .as_ref(),
            b"data",
            Element::new_item(b"deep_value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert deep item");

        // Query: from root, select level1, follow subquery_path to level2,
        // then subquery for all
        let mut inner = Query::new();
        inner.insert_all();
        let query = Query {
            items: vec![QueryItem::Key(b"level1".to_vec())],
            default_subquery_branch: SubqueryBranch {
                subquery_path: Some(vec![b"level2".to_vec()]),
                subquery: Some(Box::new(inner)),
            },
            left_to_right: true,
            conditional_subquery_branches: None,
            add_parent_tree_on_subquery: false,
        };
        let path_query = PathQuery::new_unsized(vec![b"root".to_vec()], query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1 with subquery_path");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify v1 with subquery_path");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 1, "should have 1 deep item");
    }

    #[test]
    fn prove_v1_with_limit_across_multiple_subtrees() {
        // V1 proof with a limit that spans multiple subtrees (limit cuts off mid-way)
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"root",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert root");

        for tree_name in [b"tree_a", b"tree_b", b"tree_c"] {
            db.insert(
                [b"root"].as_ref(),
                tree_name.as_slice(),
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert tree");

            for i in 0..3u8 {
                let key = format!("item_{}", i);
                db.insert(
                    [b"root", tree_name.as_slice()].as_ref(),
                    key.as_bytes(),
                    Element::new_item(vec![i]),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("should insert item");
            }
        }

        // Query all subtrees with limit 5 (should get tree_a:3 + tree_b:2)
        let mut inner = Query::new();
        inner.insert_all();
        let mut outer = Query::new();
        outer.insert_all();
        outer.set_subquery(inner);
        let path_query = PathQuery::new(
            vec![b"root".to_vec()],
            SizedQuery::new(outer, Some(5), None),
        );

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1 with limit across subtrees");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify v1 with limit across subtrees");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(
            results.len(),
            5,
            "should have exactly 5 results due to limit"
        );
    }

    #[test]
    fn prove_v1_right_to_left_with_subquery() {
        // V1 proof right-to-left direction with subquery descent
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"root",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert root");

        db.insert(
            [b"root"].as_ref(),
            b"sub",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sub tree");

        for i in 0..4u8 {
            let key = vec![b'a' + i];
            db.insert(
                [b"root".as_slice(), b"sub".as_slice()].as_ref(),
                &key,
                Element::new_item(vec![i]),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert item");
        }

        // Right-to-left with subquery
        let mut inner = Query::new();
        inner.insert_range_inclusive(vec![b'a']..=vec![b'd']);

        let query = Query {
            items: vec![QueryItem::Key(b"sub".to_vec())],
            default_subquery_branch: SubqueryBranch {
                subquery_path: None,
                subquery: Some(Box::new(inner)),
            },
            left_to_right: false, // right to left
            conditional_subquery_branches: None,
            add_parent_tree_on_subquery: false,
        };
        let path_query = PathQuery::new(
            vec![b"root".to_vec()],
            SizedQuery::new(query, Some(3), None),
        );

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1 rtl with subquery");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify v1 rtl with subquery");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 3, "should have 3 results from rtl subquery");
    }

    #[test]
    fn verify_v1_get_parent_tree_info_sum_tree() {
        // Exercise verify_query_get_parent_tree_info on a V1 proof with SumTree
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"root",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert root");

        db.insert(
            [b"root"].as_ref(),
            b"sums",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum tree");

        db.insert(
            [b"root", b"sums"].as_ref(),
            b"s1",
            Element::new_sum_item(50),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum item");

        db.insert(
            [b"root", b"sums"].as_ref(),
            b"s2",
            Element::new_sum_item(75),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum item");

        // Query the contents of the sum tree directly (no subquery)
        let mut query = Query::new();
        query.insert_all();
        let path_query = PathQuery::new_unsized(vec![b"root".to_vec(), b"sums".to_vec()], query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1 for parent tree info");

        let (root_hash, feature_type, results) =
            GroveDb::verify_query_get_parent_tree_info(&proof_bytes, &path_query, grove_version)
                .expect("should verify and get parent tree info from v1 proof");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert!(
            matches!(
                feature_type,
                grovedb_merk::TreeFeatureType::SummedMerkNode(125)
            ),
            "parent should be SummedMerkNode(125), got {:?}",
            feature_type
        );
        assert_eq!(results.len(), 2, "should have 2 sum items");
    }

    #[test]
    fn verify_v1_get_parent_tree_info_count_tree() {
        // Exercise verify_query_get_parent_tree_info with CountTree
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"root",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert root");

        db.insert(
            [b"root"].as_ref(),
            b"counts",
            Element::empty_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert count tree");

        db.insert(
            [b"root".as_slice(), b"counts".as_slice()].as_ref(),
            b"c1",
            Element::new_item(b"v1".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item in count tree");

        db.insert(
            [b"root".as_slice(), b"counts".as_slice()].as_ref(),
            b"c2",
            Element::new_item(b"v2".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item in count tree");

        let mut query = Query::new();
        query.insert_all();
        let path_query = PathQuery::new_unsized(vec![b"root".to_vec(), b"counts".to_vec()], query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove");

        let (root_hash, feature_type, results) =
            GroveDb::verify_query_get_parent_tree_info(&proof_bytes, &path_query, grove_version)
                .expect("should verify and get count tree parent info");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        // CountTree reports CountedMerkNode
        assert!(
            matches!(
                feature_type,
                grovedb_merk::TreeFeatureType::CountedMerkNode(2)
            ),
            "parent should be CountedMerkNode(2), got {:?}",
            feature_type
        );
        assert_eq!(results.len(), 2, "should have 2 items");
    }

    #[test]
    fn prove_v0_with_sum_tree_subquery() {
        // V0 proof with SumTree subquery (exercises the SumTree branch in
        // prove_subqueries V0 path)
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"root",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert root");

        db.insert(
            [b"root"].as_ref(),
            b"sum",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum tree");

        db.insert(
            [b"root".as_slice(), b"sum".as_slice()].as_ref(),
            b"entry",
            Element::new_sum_item(42),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum item");

        let mut inner = Query::new();
        inner.insert_all();
        let mut outer = Query::new();
        outer.insert_key(b"sum".to_vec());
        outer.set_subquery(inner);
        let path_query = PathQuery::new_unsized(vec![b"root".to_vec()], outer);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v0 sum tree with subquery");

        let (root_hash, results) = GroveDb::verify_query(&proof_bytes, &path_query, grove_version)
            .expect("should verify v0 sum tree subquery");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 1, "should have 1 sum item");
    }

    #[test]
    fn prove_query_many_merges_two_queries() {
        // Exercise prove_query_many with two separate path queries that get merged
        let grove_version = GroveVersion::latest();
        let db = make_deep_tree(grove_version);

        // First query: items under innertree
        let mut q1_inner = Query::new();
        q1_inner.insert_all();
        let pq1 = PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree".to_vec()], q1_inner);

        // Second query: items under innertree4
        let mut q2_inner = Query::new();
        q2_inner.insert_all();
        let pq2 =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree4".to_vec()], q2_inner);

        let proof_bytes = db
            .prove_query_many(vec![&pq1, &pq2], None, grove_version)
            .unwrap()
            .expect("should prove many with two queries");

        // The merged query should include both subtrees
        let merged_pq =
            PathQuery::merge(vec![&pq1, &pq2], grove_version).expect("should merge path queries");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &merged_pq,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify merged query proof");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        // innertree: 3 items + innertree4: 2 items = 5
        assert_eq!(
            results.len(),
            5,
            "should have 5 results from merged queries"
        );
    }

    #[test]
    fn prove_v0_tree_no_subquery_returns_tree_element() {
        // V0 proof querying a tree key without a subquery
        // The tree element itself is returned
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"root",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert root");

        db.insert(
            [b"root"].as_ref(),
            b"child",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert child tree");

        db.insert(
            [b"root".as_slice(), b"child".as_slice()].as_ref(),
            b"item",
            Element::new_item(b"inner".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        // Query the tree key without subquery
        let mut query = Query::new();
        query.insert_key(b"child".to_vec());
        let path_query = PathQuery::new_unsized(vec![b"root".to_vec()], query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove tree without subquery");

        // With include_empty_trees, should include the tree element
        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: true,
            },
            grove_version,
        )
        .expect("should verify tree without subquery");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 1, "should return the tree element itself");
    }

    #[test]
    fn prove_v0_range_query_with_limit() {
        // V0 proof with range query and limit
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        for i in 0..10u8 {
            let key = format!("k_{:02}", i);
            db.insert(
                [TEST_LEAF].as_ref(),
                key.as_bytes(),
                Element::new_item(vec![i]),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert");
        }

        // Range query with limit
        let query = Query {
            items: vec![QueryItem::RangeFull(std::ops::RangeFull)],
            left_to_right: true,
            default_subquery_branch: SubqueryBranch::default(),
            conditional_subquery_branches: None,
            add_parent_tree_on_subquery: false,
        };
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec()],
            SizedQuery::new(query, Some(4), None),
        );

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove range with limit");

        let (root_hash, results) = GroveDb::verify_query(&proof_bytes, &path_query, grove_version)
            .expect("should verify range with limit");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 4, "should have exactly 4 results from limit");
        // Verify ordering
        assert_eq!(results[0].1, b"k_00");
        assert_eq!(results[3].1, b"k_03");
    }

    #[test]
    fn prove_v1_range_query_with_limit() {
        // V1 proof with range query and limit
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");

        for i in 0..8u8 {
            let key = format!("item_{:02}", i);
            db.insert(
                [b"tree"].as_ref(),
                key.as_bytes(),
                Element::new_item(vec![i]),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert");
        }

        let query = Query {
            items: vec![QueryItem::RangeFull(std::ops::RangeFull)],
            left_to_right: true,
            default_subquery_branch: SubqueryBranch::default(),
            conditional_subquery_branches: None,
            add_parent_tree_on_subquery: false,
        };
        let path_query = PathQuery::new(
            vec![b"tree".to_vec()],
            SizedQuery::new(query, Some(3), None),
        );

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1 range with limit");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify v1 range with limit");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 3, "should have exactly 3 results from limit");
    }

    #[test]
    fn prove_v0_multi_level_three_deep() {
        // V0 proof that spans 3 levels deep
        let grove_version = GroveVersion::latest();
        let db = make_deep_tree(grove_version);

        // Query all items 3 levels deep: deep_leaf -> deep_node_1 -> deeper_1 -> items
        // and deep_leaf -> deep_node_1 -> deeper_2 -> items
        let mut level3_query = Query::new();
        level3_query.insert_all();
        let mut level2_query = Query::new();
        level2_query.insert_all();
        level2_query.set_subquery(level3_query);
        let mut level1_query = Query::new();
        level1_query.insert_key(b"deep_node_1".to_vec());
        level1_query.set_subquery(level2_query);
        let path_query = PathQuery::new_unsized(vec![b"deep_leaf".to_vec()], level1_query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove 3-level deep query");

        let (root_hash, results) = GroveDb::verify_query(&proof_bytes, &path_query, grove_version)
            .expect("should verify 3-level deep proof");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        // deeper_1 has k1,k2,k3 and deeper_2 has k4,k5,k6 = 6 total
        assert_eq!(results.len(), 6, "should have 6 items from 3-level query");
    }

    #[test]
    fn prove_v1_empty_tree_no_subquery() {
        // V1 proof querying an empty tree key without subquery
        // Exercises the Tree(None) path in v1 verification
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"root",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert root");

        // Insert an empty tree (no children)
        db.insert(
            [b"root"].as_ref(),
            b"empty_child",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert empty child tree");

        let mut query = Query::new();
        query.insert_key(b"empty_child".to_vec());
        let path_query = PathQuery::new_unsized(vec![b"root".to_vec()], query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1 empty tree no subquery");

        // With include_empty_trees, the empty tree element should be included
        let (root_hash, results_with) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: true,
            },
            grove_version,
        )
        .expect("should verify v1 empty tree with include");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(
            results_with.len(),
            1,
            "should include empty tree when include_empty_trees_in_result is true"
        );

        // Without include_empty_trees, the empty tree should be excluded
        let (_, results_without) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify v1 empty tree without include");

        assert_eq!(
            results_without.len(),
            0,
            "should not include empty tree when include_empty_trees_in_result is false"
        );
    }

    #[test]
    fn verify_v1_absence_proof_with_subquery() {
        // V1 absence proof with subquery (exercises v1 absence proof path)
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"root",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert root");

        db.insert(
            [b"root"].as_ref(),
            b"sub",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sub");

        db.insert(
            [b"root".as_slice(), b"sub".as_slice()].as_ref(),
            b"real",
            Element::new_item(b"rv".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        // Query with subquery, one existing and one missing key, with limit
        let mut inner = Query::new();
        inner.insert_key(b"real".to_vec());
        inner.insert_key(b"ghost".to_vec());
        let mut outer = Query::new();
        outer.insert_key(b"sub".to_vec());
        outer.set_subquery(inner);
        let path_query = PathQuery::new(
            vec![b"root".to_vec()],
            SizedQuery::new(outer, Some(5), None),
        );

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1 absence with subquery");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: true,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify v1 absence proof with subquery");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 2, "should have 2 entries (present + absent)");

        let real_entry = results.iter().find(|(_, k, _)| k == b"real");
        assert!(real_entry.is_some(), "should find 'real' key");
        assert!(real_entry.unwrap().2.is_some(), "'real' should have value");

        let ghost_entry = results.iter().find(|(_, k, _)| k == b"ghost");
        assert!(ghost_entry.is_some(), "should find 'ghost' key");
        assert!(ghost_entry.unwrap().2.is_none(), "'ghost' should be absent");
    }

    #[test]
    fn prove_v0_on_bulk_append_tree_errors() {
        // V0 proofs should error when encountering BulkAppendTree with subquery
        let grove_version = &GROVE_V2;
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"bat",
            Element::empty_bulk_append_tree(2).expect("valid chunk_power"),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert bulk append tree");

        db.bulk_append(EMPTY_PATH, b"bat", vec![1, 2, 3], None, grove_version)
            .unwrap()
            .expect("should append");

        // Try V0 query with subquery into BulkAppendTree
        let mut inner = Query::new();
        inner.insert_key(0u64.to_be_bytes().to_vec());
        let query = Query {
            items: vec![QueryItem::Key(b"bat".to_vec())],
            default_subquery_branch: SubqueryBranch {
                subquery_path: None,
                subquery: Some(Box::new(inner)),
            },
            left_to_right: true,
            conditional_subquery_branches: None,
            add_parent_tree_on_subquery: false,
        };
        let path_query = PathQuery::new_unsized(vec![], query);

        let result = db.prove_query(&path_query, None, grove_version).unwrap();
        assert!(
            result.is_err(),
            "V0 proofs should not support BulkAppendTree subqueries"
        );
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("V0 proofs do not support"),
            "error should mention V0 limitation: {}",
            err_msg
        );
    }

    #[test]
    fn prove_v0_on_dense_tree_errors() {
        // V0 proofs should error when encountering DenseTree with subquery
        let grove_version = &GROVE_V2;
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"dense",
            Element::empty_dense_tree(3),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert dense tree");

        db.dense_tree_insert(EMPTY_PATH, b"dense", vec![99], None, grove_version)
            .unwrap()
            .expect("should insert into dense tree");

        // Try V0 query with subquery into DenseTree
        let mut inner = Query::new();
        inner.insert_key(0u16.to_be_bytes().to_vec());
        let query = Query {
            items: vec![QueryItem::Key(b"dense".to_vec())],
            default_subquery_branch: SubqueryBranch {
                subquery_path: None,
                subquery: Some(Box::new(inner)),
            },
            left_to_right: true,
            conditional_subquery_branches: None,
            add_parent_tree_on_subquery: false,
        };
        let path_query = PathQuery::new_unsized(vec![], query);

        let result = db.prove_query(&path_query, None, grove_version).unwrap();
        assert!(
            result.is_err(),
            "V0 proofs should not support DenseTree subqueries"
        );
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("V0 proofs do not support"),
            "error should mention V0 limitation: {}",
            err_msg
        );
    }

    #[test]
    fn prove_v1_mmr_tree_no_subquery() {
        // V1 proof querying MmrTree key without subquery
        // Tree itself counted as a result
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"root",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert root");

        db.insert(
            [b"root"].as_ref(),
            b"mmr",
            Element::empty_mmr_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert mmr tree");

        db.mmr_tree_append(
            [b"root"].as_ref(),
            b"mmr",
            b"data".to_vec(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("should append to mmr");

        // Query the mmr key without subquery
        let mut query = Query::new();
        query.insert_key(b"mmr".to_vec());
        let path_query = PathQuery::new_unsized(vec![b"root".to_vec()], query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1 mmr tree no subquery");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: true,
            },
            grove_version,
        )
        .expect("should verify v1 mmr tree no subquery");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(
            results.len(),
            1,
            "should return the mmr tree element itself"
        );
    }

    #[test]
    fn prove_v1_bulk_append_tree_no_subquery() {
        // V1 proof querying BulkAppendTree key without subquery
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"root",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert root");

        db.insert(
            [b"root"].as_ref(),
            b"bat",
            Element::empty_bulk_append_tree(2).expect("valid chunk_power"),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert bulk append tree");

        db.bulk_append(
            [b"root"].as_ref(),
            b"bat",
            vec![1, 2, 3],
            None,
            grove_version,
        )
        .unwrap()
        .expect("should append");

        let mut query = Query::new();
        query.insert_key(b"bat".to_vec());
        let path_query = PathQuery::new_unsized(vec![b"root".to_vec()], query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1 bat no subquery");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: true,
            },
            grove_version,
        )
        .expect("should verify v1 bat no subquery");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 1, "should return bat element itself");
    }

    #[test]
    fn prove_v1_mixed_merk_and_non_merk_subtrees() {
        // V1 proof with a mix of Merk subtree and non-Merk (MMR) subtree
        // at the same level, both queried with subqueries
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"root",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert root");

        // Merk subtree
        db.insert(
            [b"root"].as_ref(),
            b"merk_sub",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert merk subtree");

        db.insert(
            [b"root".as_slice(), b"merk_sub".as_slice()].as_ref(),
            b"m1",
            Element::new_item(b"mval1".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        // MMR subtree
        db.insert(
            [b"root"].as_ref(),
            b"mmr_sub",
            Element::empty_mmr_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert mmr subtree");

        db.mmr_tree_append(
            [b"root"].as_ref(),
            b"mmr_sub",
            b"mmr_val1".to_vec(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("should append to mmr");

        // Build query that gets all subtrees and descends into them
        let mut inner = Query::new();
        inner.insert_all();
        let mut outer = Query::new();
        outer.insert_all();
        outer.set_subquery(inner);
        let path_query = PathQuery::new_unsized(vec![b"root".to_vec()], outer);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1 mixed subtrees");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify v1 mixed subtrees");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        // merk_sub: 1 item + mmr_sub: 1 MMR leaf = 2
        assert_eq!(
            results.len(),
            2,
            "should have 2 results from mixed Merk+MMR subtrees"
        );
    }

    #[test]
    fn prove_v0_reference_in_subtree() {
        // V0 proof with a reference in a subtree that points to an item
        // in the same subtree (sibling reference)
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"root",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert root");

        db.insert(
            [b"root"].as_ref(),
            b"sub",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sub");

        db.insert(
            [b"root".as_slice(), b"sub".as_slice()].as_ref(),
            b"target",
            Element::new_item(b"resolved_value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert target");

        db.insert(
            [b"root".as_slice(), b"sub".as_slice()].as_ref(),
            b"ref",
            Element::new_reference(crate::reference_path::ReferencePathType::SiblingReference(
                b"target".to_vec(),
            )),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert reference");

        let mut inner = Query::new();
        inner.insert_key(b"ref".to_vec());
        let mut outer = Query::new();
        outer.insert_key(b"sub".to_vec());
        outer.set_subquery(inner);
        let path_query = PathQuery::new_unsized(vec![b"root".to_vec()], outer);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v0 ref in subtree");

        let (root_hash, results) = GroveDb::verify_query(&proof_bytes, &path_query, grove_version)
            .expect("should verify v0 ref in subtree");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 1, "should resolve reference");
        let elem = results[0].2.as_ref().expect("should have element");
        match elem {
            Element::Item(data, _) => {
                assert_eq!(
                    data,
                    &b"resolved_value".to_vec(),
                    "reference should resolve to target value"
                );
            }
            _ => panic!("expected Item, got {:?}", elem),
        }
    }

    #[test]
    fn prove_v1_reference_in_subtree() {
        // V1 proof with a reference in a subtree
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"root",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert root");

        db.insert(
            [b"root"].as_ref(),
            b"sub",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sub");

        db.insert(
            [b"root".as_slice(), b"sub".as_slice()].as_ref(),
            b"origin",
            Element::new_item(b"origin_data".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert origin");

        db.insert(
            [b"root".as_slice(), b"sub".as_slice()].as_ref(),
            b"ptr",
            Element::new_reference(crate::reference_path::ReferencePathType::SiblingReference(
                b"origin".to_vec(),
            )),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert reference");

        let mut inner = Query::new();
        inner.insert_key(b"ptr".to_vec());
        let mut outer = Query::new();
        outer.insert_key(b"sub".to_vec());
        outer.set_subquery(inner);
        let path_query = PathQuery::new_unsized(vec![b"root".to_vec()], outer);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1 ref in subtree");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify v1 ref in subtree");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 1, "should resolve reference");
        let elem = results[0].2.as_ref().expect("should have element");
        match elem {
            Element::Item(data, _) => {
                assert_eq!(data, &b"origin_data".to_vec());
            }
            _ => panic!("expected Item, got {:?}", elem),
        }
    }

    #[test]
    fn verify_v0_get_parent_tree_info_with_offset_errors() {
        // verify_query_get_parent_tree_info_with_options should error on offset
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"item",
            Element::new_item(b"v".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert");

        let mut query = Query::new();
        query.insert_all();
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec()],
            SizedQuery::new(query, None, Some(1)),
        );

        let proof_bytes = db
            .prove_query(
                &PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], {
                    let mut q = Query::new();
                    q.insert_all();
                    q
                }),
                None,
                grove_version,
            )
            .unwrap()
            .expect("should prove");

        let result = GroveDb::verify_query_get_parent_tree_info_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        );
        assert!(result.is_err(), "parent tree info with offset should error");
    }

    #[test]
    fn verify_v0_get_parent_tree_info_absence_without_limit_errors() {
        // verify_query_get_parent_tree_info_with_options should error when
        // absence_proofs requested without a limit
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"item",
            Element::new_item(b"v".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert");

        let mut query = Query::new();
        query.insert_all();
        // No limit
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove");

        let result = GroveDb::verify_query_get_parent_tree_info_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: true,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        );
        assert!(
            result.is_err(),
            "parent tree info with absence without limit should error"
        );
    }

    #[test]
    fn verify_get_parent_tree_info_for_root_query_errors() {
        // verify_query_get_parent_tree_info should error when querying
        // at the root level (no parent tree info)
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let mut query = Query::new();
        query.insert_all();
        let path_query = PathQuery::new_unsized(vec![], query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove root query");

        let result =
            GroveDb::verify_query_get_parent_tree_info(&proof_bytes, &path_query, grove_version);
        assert!(result.is_err(), "root-level query has no parent tree info");
    }

    #[test]
    fn prove_v1_with_item_with_sum_item() {
        // V1 proof with ItemWithSumItem element type
        // Exercises the ItemWithSumItem match arms in proof generation/verification
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"root",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert root");

        db.insert(
            [b"root"].as_ref(),
            b"sum_tree",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum tree");

        // ItemWithSumItem: both regular data and a sum contribution
        db.insert(
            [b"root".as_slice(), b"sum_tree".as_slice()].as_ref(),
            b"hybrid",
            Element::new_item_with_sum_item(b"payload".to_vec(), 42),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item_with_sum_item");

        let mut inner = Query::new();
        inner.insert_all();
        let mut outer = Query::new();
        outer.insert_key(b"sum_tree".to_vec());
        outer.set_subquery(inner);
        let path_query = PathQuery::new_unsized(vec![b"root".to_vec()], outer);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1 with item_with_sum_item");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify v1 with item_with_sum_item");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 1, "should have 1 result");
    }

    #[test]
    fn prove_v0_verify_succinctness() {
        // Exercise verify_proof_succinctness option
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"only_key",
            Element::new_item(b"only_val".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert");

        let mut query = Query::new();
        query.insert_key(b"only_key".to_vec());
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: true,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify with succinctness check");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 1, "should have 1 result");
    }

    #[test]
    fn prove_v1_verify_succinctness() {
        // Exercise verify_proof_succinctness on V1 proof
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");

        db.insert(
            [b"tree"].as_ref(),
            b"k",
            Element::new_item(b"v".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        let mut query = Query::new();
        query.insert_key(b"k".to_vec());
        let path_query = PathQuery::new_unsized(vec![b"tree".to_vec()], query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove v1");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: true,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify v1 with succinctness");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn verify_v1_subset_query() {
        // Exercise subset verification on a V1 proof
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");

        for i in 0..5u8 {
            db.insert(
                [b"tree"].as_ref(),
                &[b'a' + i],
                Element::new_item(vec![i]),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert item");
        }

        // Full proof
        let mut full_query = Query::new();
        full_query.insert_all();
        let full_pq = PathQuery::new_unsized(vec![b"tree".to_vec()], full_query);

        let proof_bytes = db
            .prove_query(&full_pq, None, grove_version)
            .unwrap()
            .expect("should prove v1 full");

        // Subset query for 2 keys
        let mut subset_query = Query::new();
        subset_query.insert_key(vec![b'b']);
        subset_query.insert_key(vec![b'd']);
        let subset_pq = PathQuery::new_unsized(vec![b"tree".to_vec()], subset_query);

        let (root_hash, results) =
            GroveDb::verify_subset_query(&proof_bytes, &subset_pq, grove_version)
                .expect("should verify v1 subset");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert_eq!(results.len(), 2, "subset should return 2 results");
    }

    #[test]
    fn prove_v0_deep_tree_with_sum_trees() {
        // Exercise the make_deep_tree_with_sum_trees helper and prove/verify
        // a complex query with SumTrees at various levels
        let grove_version = GroveVersion::latest();
        let db = crate::tests::make_deep_tree_with_sum_trees(grove_version);

        // Query: deep_leaf -> deep_node_1 -> c -> 1 (sum tree) -> all items
        let mut leaf_query = Query::new();
        leaf_query.insert_all();
        let mut sum_query = Query::new();
        sum_query.insert_key(b"1".to_vec());
        sum_query.set_subquery(leaf_query);
        let mut inner_query = Query::new();
        inner_query.insert_key(b"c".to_vec());
        inner_query.set_subquery(sum_query);
        let path_query = PathQuery::new_unsized(
            vec![b"deep_leaf".to_vec(), b"deep_node_1".to_vec()],
            inner_query,
        );

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove deep tree with sum trees");

        let (root_hash, results) = GroveDb::verify_query(&proof_bytes, &path_query, grove_version)
            .expect("should verify deep tree with sum trees");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        // c -> 1 (sum tree) has [0;32] -> 1 and [1;32] -> 1
        assert_eq!(
            results.len(),
            2,
            "should have 2 sum items from deep sum tree"
        );
    }

    // =========================================================================
    // BulkAppendTree query variant coverage
    //
    // Exercises different QueryItem match arms in verify.rs:
    //   extract_range_from_query_items (lines 993-1111)
    //   expand_query_to_u64_positions  (lines 1116-1224)
    //
    // Existing tests only use Key and RangeInclusive. These tests cover:
    //   Range, RangeFull, RangeFrom, RangeTo, RangeToInclusive,
    //   RangeAfter, RangeAfterTo, RangeAfterToInclusive
    // =========================================================================

    /// Helper: create a BulkAppendTree at path [b"root", b"bat"] with `count`
    /// items (values [50, 51, ...]) and return the db handle.
    fn setup_bulk_append_tree(
        grove_version: &GroveVersion,
        count: u8,
    ) -> crate::tests::TempGroveDb {
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"root",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert root");

        db.insert(
            [b"root"].as_ref(),
            b"bat",
            Element::empty_bulk_append_tree(2).expect("valid chunk_power"), // chunk_power = 2 (epoch_size = 4)
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert bulk append tree");

        for i in 0..count {
            db.bulk_append(
                [b"root"].as_ref(),
                b"bat",
                vec![i + 50],
                None,
                grove_version,
            )
            .unwrap()
            .expect("append value");
        }

        db
    }

    /// Helper: build a PathQuery that queries a BulkAppendTree at
    /// [b"root"] -> key b"bat" -> inner_query.
    fn bat_path_query(inner_query: Query) -> PathQuery {
        let query = Query {
            items: vec![QueryItem::Key(b"bat".to_vec())],
            default_subquery_branch: SubqueryBranch {
                subquery_path: None,
                subquery: Some(Box::new(inner_query)),
            },
            left_to_right: true,
            conditional_subquery_branches: None,
            add_parent_tree_on_subquery: false,
        };
        PathQuery::new_unsized(vec![b"root".to_vec()], query)
    }

    /// Helper: prove and verify a BulkAppendTree query, returning result count.
    fn prove_verify_bat(
        db: &crate::tests::TempGroveDb,
        path_query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> usize {
        let proof_bytes = db
            .prove_query(path_query, None, grove_version)
            .unwrap()
            .expect("should prove");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root, "root hash mismatch");
        results.len()
    }

    #[test]
    fn bulk_append_query_range_exclusive() {
        // QueryItem::Range (exclusive end): positions 1..4
        let grove_version = GroveVersion::latest();
        let db = setup_bulk_append_tree(grove_version, 6);

        let mut inner = Query::new();
        inner.insert_range(1u64.to_be_bytes().to_vec()..4u64.to_be_bytes().to_vec());
        let pq = bat_path_query(inner);

        let count = prove_verify_bat(&db, &pq, grove_version);
        assert_eq!(count, 3, "range 1..4 should return positions 1,2,3");
    }

    #[test]
    fn bulk_append_query_range_full() {
        // QueryItem::RangeFull: all positions
        let grove_version = GroveVersion::latest();
        let db = setup_bulk_append_tree(grove_version, 6);

        let mut inner = Query::new();
        inner.insert_all();
        let pq = bat_path_query(inner);

        let count = prove_verify_bat(&db, &pq, grove_version);
        assert_eq!(count, 6, "RangeFull should return all 6 items");
    }

    #[test]
    fn bulk_append_query_range_from() {
        // QueryItem::RangeFrom: positions 3..
        let grove_version = GroveVersion::latest();
        let db = setup_bulk_append_tree(grove_version, 6);

        let mut inner = Query::new();
        inner.insert_range_from(3u64.to_be_bytes().to_vec()..);
        let pq = bat_path_query(inner);

        let count = prove_verify_bat(&db, &pq, grove_version);
        assert_eq!(count, 3, "RangeFrom 3.. should return positions 3,4,5");
    }

    #[test]
    fn bulk_append_query_range_to() {
        // QueryItem::RangeTo: ..3 (positions 0,1,2)
        let grove_version = GroveVersion::latest();
        let db = setup_bulk_append_tree(grove_version, 6);

        let mut inner = Query::new();
        inner.insert_range_to(..3u64.to_be_bytes().to_vec());
        let pq = bat_path_query(inner);

        let count = prove_verify_bat(&db, &pq, grove_version);
        assert_eq!(count, 3, "RangeTo ..3 should return positions 0,1,2");
    }

    #[test]
    fn bulk_append_query_range_to_inclusive() {
        // QueryItem::RangeToInclusive: ..=3 (positions 0,1,2,3)
        let grove_version = GroveVersion::latest();
        let db = setup_bulk_append_tree(grove_version, 6);

        let mut inner = Query::new();
        inner.insert_range_to_inclusive(..=3u64.to_be_bytes().to_vec());
        let pq = bat_path_query(inner);

        let count = prove_verify_bat(&db, &pq, grove_version);
        assert_eq!(count, 4, "RangeToInclusive ..=3 should return 4 items");
    }

    #[test]
    fn bulk_append_query_range_after() {
        // QueryItem::RangeAfter: (2, ∞) — positions 3,4,5
        let grove_version = GroveVersion::latest();
        let db = setup_bulk_append_tree(grove_version, 6);

        let mut inner = Query::new();
        inner.insert_range_after(2u64.to_be_bytes().to_vec()..);
        let pq = bat_path_query(inner);

        let count = prove_verify_bat(&db, &pq, grove_version);
        assert_eq!(count, 3, "RangeAfter (2,∞) should return positions 3,4,5");
    }

    #[test]
    fn bulk_append_query_range_after_to() {
        // QueryItem::RangeAfterTo: (1, 5) — positions 2,3,4
        let grove_version = GroveVersion::latest();
        let db = setup_bulk_append_tree(grove_version, 6);

        let mut inner = Query::new();
        inner.insert_range_after_to(1u64.to_be_bytes().to_vec()..5u64.to_be_bytes().to_vec());
        let pq = bat_path_query(inner);

        let count = prove_verify_bat(&db, &pq, grove_version);
        assert_eq!(count, 3, "RangeAfterTo (1,5) should return positions 2,3,4");
    }

    #[test]
    fn bulk_append_query_range_after_to_inclusive() {
        // QueryItem::RangeAfterToInclusive: (1, 4] — positions 2,3,4
        let grove_version = GroveVersion::latest();
        let db = setup_bulk_append_tree(grove_version, 6);

        let mut inner = Query::new();
        inner.insert_range_after_to_inclusive(
            1u64.to_be_bytes().to_vec()..=4u64.to_be_bytes().to_vec(),
        );
        let pq = bat_path_query(inner);

        let count = prove_verify_bat(&db, &pq, grove_version);
        assert_eq!(count, 3, "RangeAfterToInclusive (1,4] should return 2,3,4");
    }

    // =========================================================================
    // QueryItem variant coverage for MMR, Dense, and CommitmentTree types
    //
    // Each test sets up a non-Merk tree with several items, then loops through
    // the 8 previously-untested QueryItem variants building a proof for each.
    // This covers the match arms in:
    //   generate.rs: query_items_to_positions, query_items_to_leaf_indices,
    //                query_items_to_range
    //   verify.rs:   extract_range_from_query_items, expand_query_to_u64_positions
    // =========================================================================

    /// Helper: build a PathQuery selecting `tree_key` inside `[b"root"]`,
    /// with a single QueryItem as the subquery.
    fn make_subquery_path_query(tree_key: &[u8], subquery_item: QueryItem) -> PathQuery {
        let inner = Query {
            items: vec![subquery_item],
            default_subquery_branch: SubqueryBranch {
                subquery_path: None,
                subquery: None,
            },
            left_to_right: true,
            conditional_subquery_branches: None,
            add_parent_tree_on_subquery: false,
        };
        let query = Query {
            items: vec![QueryItem::Key(tree_key.to_vec())],
            default_subquery_branch: SubqueryBranch {
                subquery_path: None,
                subquery: Some(Box::new(inner)),
            },
            left_to_right: true,
            conditional_subquery_branches: None,
            add_parent_tree_on_subquery: false,
        };
        PathQuery::new_unsized(vec![b"root".to_vec()], query)
    }

    /// Helper: prove and verify a PathQuery, returning the number of results.
    fn prove_and_verify_v1(
        db: &crate::GroveDb,
        path_query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> usize {
        let proof_bytes = db
            .prove_query(path_query, None, grove_version)
            .unwrap()
            .expect("should generate v1 proof");

        let (root_hash, results) = GroveDb::verify_query_with_options(
            &proof_bytes,
            path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .expect("should verify v1 proof");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root, "root hash mismatch");
        results.len()
    }

    #[test]
    fn prove_v1_mmr_tree_query_variants() {
        // Exercises 8 QueryItem range variants against an MmrTree.
        // Covers generate.rs::query_items_to_leaf_indices and
        // verify.rs::expand_query_to_u64_positions.
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"root",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert root");

        db.insert(
            [b"root"].as_ref(),
            b"mmr",
            Element::empty_mmr_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert mmr tree");

        // Append 8 leaves (leaf indices 0..8)
        for i in 0..8u8 {
            db.mmr_tree_append(
                [b"root"].as_ref(),
                b"mmr",
                vec![i + 100],
                None,
                grove_version,
            )
            .unwrap()
            .expect("append to mmr");
        }

        let be = |v: u64| -> Vec<u8> { v.to_be_bytes().to_vec() };

        let variants: Vec<(&str, QueryItem, usize)> = vec![
            // Range: leaf indices 2..5 → 2, 3, 4
            ("Range", QueryItem::Range(be(2)..be(5)), 3),
            // RangeFrom: 5.. → 5, 6, 7
            ("RangeFrom", QueryItem::RangeFrom(be(5)..), 3),
            // RangeTo: ..3 → 0, 1, 2
            ("RangeTo", QueryItem::RangeTo(..be(3)), 3),
            // RangeToInclusive: ..=2 → 0, 1, 2
            ("RangeToInclusive", QueryItem::RangeToInclusive(..=be(2)), 3),
            // RangeFull: all → 0..8
            ("RangeFull", QueryItem::RangeFull(std::ops::RangeFull), 8),
            // RangeAfter: after 4 → 5, 6, 7
            ("RangeAfter", QueryItem::RangeAfter(be(4)..), 3),
            // RangeAfterTo: after 1, before 5 → 2, 3, 4
            ("RangeAfterTo", QueryItem::RangeAfterTo(be(1)..be(5)), 3),
            // RangeAfterToInclusive: after 1, through 4 → 2, 3, 4
            (
                "RangeAfterToInclusive",
                QueryItem::RangeAfterToInclusive(be(1)..=be(4)),
                3,
            ),
        ];

        for (name, item, expected_count) in variants {
            let pq = make_subquery_path_query(b"mmr", item);
            let count = prove_and_verify_v1(&db, &pq, grove_version);
            assert_eq!(
                count, expected_count,
                "MMR variant {name}: expected {expected_count} results, got {count}"
            );
        }
    }

    #[test]
    fn prove_v1_dense_tree_query_variants() {
        // Exercises 8 QueryItem range variants against a DenseAppendOnlyFixedSizeTree.
        // Covers generate.rs::query_items_to_positions.
        // Keys are BE u16 (2 bytes).
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"root",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert root");

        db.insert(
            [b"root"].as_ref(),
            b"dense",
            Element::empty_dense_tree(4), // height=4, max 16 entries
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert dense tree");

        // Insert 8 values (positions 0..8)
        for i in 0..8u16 {
            db.dense_tree_insert(
                [b"root"].as_ref(),
                b"dense",
                vec![i as u8 + 10],
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert into dense tree");
        }

        let be = |v: u16| -> Vec<u8> { v.to_be_bytes().to_vec() };

        let variants: Vec<(&str, QueryItem, usize)> = vec![
            // Range: positions 1..4 → 1, 2, 3
            ("Range", QueryItem::Range(be(1)..be(4)), 3),
            // RangeFrom: 5.. → 5, 6, 7
            ("RangeFrom", QueryItem::RangeFrom(be(5)..), 3),
            // RangeTo: ..3 → 0, 1, 2
            ("RangeTo", QueryItem::RangeTo(..be(3)), 3),
            // RangeToInclusive: ..=2 → 0, 1, 2
            ("RangeToInclusive", QueryItem::RangeToInclusive(..=be(2)), 3),
            // RangeFull: all → 0..8
            ("RangeFull", QueryItem::RangeFull(std::ops::RangeFull), 8),
            // RangeAfter: after 4 → 5, 6, 7
            ("RangeAfter", QueryItem::RangeAfter(be(4)..), 3),
            // RangeAfterTo: after 1, before 5 → 2, 3, 4
            ("RangeAfterTo", QueryItem::RangeAfterTo(be(1)..be(5)), 3),
            // RangeAfterToInclusive: after 1, through 4 → 2, 3, 4
            (
                "RangeAfterToInclusive",
                QueryItem::RangeAfterToInclusive(be(1)..=be(4)),
                3,
            ),
        ];

        for (name, item, expected_count) in variants {
            let pq = make_subquery_path_query(b"dense", item);
            let count = prove_and_verify_v1(&db, &pq, grove_version);
            assert_eq!(
                count, expected_count,
                "DenseTree variant {name}: expected {expected_count} results, got {count}"
            );
        }
    }

    #[test]
    fn prove_v1_commitment_tree_query_variants() {
        // Exercises 8 QueryItem range variants against a CommitmentTree.
        // CommitmentTree wraps a BulkAppendTree + Sinsemilla frontier.
        // Covers verify.rs::verify_commitment_tree_lower_layer and all
        // BulkAppend generate/verify paths.
        // Keys are BE u64 (8 bytes), same as BulkAppendTree.
        use grovedb_commitment_tree::{DashMemo, NoteBytesData, TransmittedNoteCiphertext};

        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"root",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert root");

        // chunk_power=2 so compaction happens at 4 items (exercises chunked paths)
        db.insert(
            [b"root"].as_ref(),
            b"pool",
            Element::empty_commitment_tree(2).expect("valid chunk_power"),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert commitment tree");

        // Helper: deterministic 32-byte cmx from index (valid Pallas field element)
        fn ct_cmx(index: u8) -> [u8; 32] {
            let mut bytes = [0u8; 32];
            bytes[0] = index;
            bytes[31] &= 0x7f;
            bytes
        }

        fn ct_rho(index: u8) -> [u8; 32] {
            let mut bytes = [0u8; 32];
            bytes[0] = index;
            bytes[1] = 0xAA;
            bytes
        }

        fn ct_ciphertext(index: u8) -> TransmittedNoteCiphertext<DashMemo> {
            let mut epk_bytes = [0u8; 32];
            epk_bytes[0] = index;
            epk_bytes[1] = index.wrapping_add(1);
            let mut enc_data = [0u8; 104];
            enc_data[0] = index;
            enc_data[1] = 0xEC;
            let enc_ciphertext = NoteBytesData(enc_data);
            let mut out_ciphertext = [0u8; 80];
            out_ciphertext[0] = index;
            out_ciphertext[1] = 0x0C;
            TransmittedNoteCiphertext::from_parts(epk_bytes, enc_ciphertext, out_ciphertext)
        }

        // Insert 6 notes (positions 0..6); chunk_power=2 means epoch_size=4,
        // so we get at least one compacted chunk + buffer items.
        for i in 0..6u8 {
            db.commitment_tree_insert(
                [b"root"].as_ref(),
                b"pool",
                ct_cmx(i),
                ct_rho(i),
                ct_ciphertext(i),
                None,
                grove_version,
            )
            .unwrap()
            .expect("commitment tree insert");
        }

        let be = |v: u64| -> Vec<u8> { v.to_be_bytes().to_vec() };

        let variants: Vec<(&str, QueryItem, usize)> = vec![
            ("Range", QueryItem::Range(be(1)..be(4)), 3),
            ("RangeFrom", QueryItem::RangeFrom(be(3)..), 3),
            ("RangeTo", QueryItem::RangeTo(..be(3)), 3),
            ("RangeToInclusive", QueryItem::RangeToInclusive(..=be(2)), 3),
            ("RangeFull", QueryItem::RangeFull(std::ops::RangeFull), 6),
            ("RangeAfter", QueryItem::RangeAfter(be(1)..), 4),
            ("RangeAfterTo", QueryItem::RangeAfterTo(be(0)..be(4)), 3),
            (
                "RangeAfterToInclusive",
                QueryItem::RangeAfterToInclusive(be(0)..=be(3)),
                3,
            ),
        ];

        for (name, item, expected_count) in variants {
            let pq = make_subquery_path_query(b"pool", item);
            let count = prove_and_verify_v1(&db, &pq, grove_version);
            assert_eq!(
                count, expected_count,
                "CommitmentTree variant {name}: expected {expected_count} results, got {count}"
            );
        }
    }

    #[test]
    fn prove_v0_mixed_elements_right_to_left_with_limit() {
        // V0 proof right-to-left over a subtree with items, trees, and references
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"root",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert root");

        db.insert(
            [b"root"].as_ref(),
            b"sub",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert sub");

        // Mix of element types in the subtree
        db.insert(
            [b"root".as_slice(), b"sub".as_slice()].as_ref(),
            b"a_item",
            Element::new_item(b"val_a".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item a");

        db.insert(
            [b"root".as_slice(), b"sub".as_slice()].as_ref(),
            b"b_tree",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert tree b");

        db.insert(
            [b"root".as_slice(), b"sub".as_slice()].as_ref(),
            b"c_item",
            Element::new_item(b"val_c".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item c");

        db.insert(
            [b"root".as_slice(), b"sub".as_slice()].as_ref(),
            b"d_ref",
            Element::new_reference(crate::reference_path::ReferencePathType::SiblingReference(
                b"a_item".to_vec(),
            )),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert ref d");

        // Right-to-left query with limit=3
        let mut inner = Query::new_with_direction(false);
        inner.insert_all();
        let mut outer = Query::new();
        outer.insert_key(b"sub".to_vec());
        outer.set_subquery(inner);
        let path_query = PathQuery {
            path: vec![b"root".to_vec()],
            query: SizedQuery {
                query: outer,
                limit: Some(3),
                offset: None,
            },
        };

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove right-to-left mixed");

        let (root_hash, results) = GroveDb::verify_query(&proof_bytes, &path_query, grove_version)
            .expect("should verify right-to-left mixed");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        // d_ref (limit 3→2), c_item (2→1), b_tree (empty subquery, 1→0, done)
        // So only 2 actual results are returned (the empty subtree consumes
        // a limit slot with default decrease_limit_on_empty=true)
        assert_eq!(
            results.len(),
            2,
            "right-to-left with limit=3: d_ref + c_item (b_tree empty consumes limit)"
        );
    }

    #[test]
    fn verify_no_underflow_when_limit_zero_and_empty_subtree() {
        // Regression test for H4: u16 limit underflow in proof verification.
        //
        // When `decrease_limit_on_empty_sub_query_result` is true and the
        // limit reaches 0 after processing one empty subtree, encountering
        // another empty subtree must NOT wrap `0u16 - 1` to 65535.
        //
        // Setup: create multiple empty subtrees under TEST_LEAF so that
        // with limit=1, the first empty subtree decrements limit to 0
        // and the second one would have caused underflow before the fix.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert three empty subtrees under TEST_LEAF.
        // With limit=1 and decrease_limit_on_empty=true, the first empty
        // subtree consumes the limit (1->0). Before the fix, the second
        // empty subtree would attempt 0u16 - 1, causing underflow.
        for key in [b"empty_1".as_slice(), b"empty_2", b"empty_3"] {
            db.insert(
                [TEST_LEAF].as_ref(),
                key,
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert empty subtree");
        }

        let mut inner = Query::new();
        inner.insert_all();
        let mut outer = Query::new();
        outer.insert_all();
        outer.set_subquery(inner);
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec()],
            SizedQuery::new(outer, Some(1), None),
        );

        let options = ProveOptions {
            decrease_limit_on_empty_sub_query_result: true,
        };

        // Generate proof with the same limit and options
        let proof_bytes = db
            .prove_query(&path_query, Some(options), grove_version)
            .unwrap()
            .expect("should prove with limit=1 and empty subtrees");

        // Before the fix, this verification would panic in debug mode
        // (attempt to subtract with overflow) or silently wrap limit_left
        // from 0 to 65535 in release mode, accepting oversized result sets.
        let verify_result = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        );

        assert!(
            verify_result.is_ok(),
            "verification should not panic or error on limit underflow: {:?}",
            verify_result.err()
        );

        let (root_hash, results) = verify_result.unwrap();
        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        // With limit=1 and decrease_limit_on_empty=true, only one empty
        // subtree should consume the limit, yielding 0 actual results
        // (since empty subtrees produce no result items).
        assert!(
            results.len() <= 1,
            "with limit=1, should have at most 1 result, got {}",
            results.len()
        );
    }

    #[test]
    fn verify_v1_no_underflow_when_limit_zero_and_empty_subtree() {
        // Same regression test as above but for V1 proof path.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        for key in [b"empty_v1_1".as_slice(), b"empty_v1_2", b"empty_v1_3"] {
            db.insert(
                [TEST_LEAF].as_ref(),
                key,
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert empty subtree");
        }

        let mut inner = Query::new();
        inner.insert_all();
        let mut outer = Query::new();
        outer.insert_all();
        outer.set_subquery(inner);
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec()],
            SizedQuery::new(outer, Some(1), None),
        );

        let options = ProveOptions {
            decrease_limit_on_empty_sub_query_result: true,
        };

        let proof_bytes = db
            .prove_query(&path_query, Some(options), grove_version)
            .unwrap()
            .expect("should prove v1 with limit=1 and empty subtrees");

        let verify_result = GroveDb::verify_query_with_options(
            &proof_bytes,
            &path_query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        );

        assert!(
            verify_result.is_ok(),
            "v1 verification should not panic or error on limit underflow: {:?}",
            verify_result.err()
        );

        let (root_hash, results) = verify_result.unwrap();
        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root);
        assert!(
            results.len() <= 1,
            "v1: with limit=1, should have at most 1 result, got {}",
            results.len()
        );
    }

    /// Demonstrates the KV→KVValueHash proof forgery attack (C1).
    ///
    /// An attacker intercepts a valid proof containing `KV(key, real_value)`
    /// (tag 0x03) and replaces it with `KVValueHash(key, fake_value,
    /// value_hash(real_value))` (tag 0x04). The Merkle root stays valid
    /// because the hash computation is algebraically identical, but the
    /// verifier returns `fake_value` instead of `real_value`.
    ///
    /// The fix adds a check in GroveDB's verify layer: for item elements where
    /// the value_hash was provided by the proof (not computed by the verifier),
    /// independently compute value_hash(value) and check it matches.
    #[test]
    fn kv_to_kvvaluehash_forgery_is_detected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let real_value = b"real_secret_value".to_vec();
        let fake_value = b"attacker_controlled".to_vec();

        db.insert(
            [TEST_LEAF].as_ref(),
            b"mykey",
            Element::new_item(real_value.clone()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        let mut query = Query::new();
        query.insert_key(b"mykey".to_vec());
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        // Generate a valid proof
        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should generate proof");

        // Sanity: the valid proof verifies correctly
        let valid_result = GroveDb::verify_query_raw(&proof_bytes, &path_query, grove_version);
        assert!(valid_result.is_ok(), "valid proof should verify");

        // === Tamper the proof ===
        // The inner merk proof contains a KV node (tag 0x03) for the leaf item.
        // We replace it with KVValueHash (tag 0x04) keeping the real value_hash
        // but swapping in a fake value.
        //
        // Compute the real value_hash. The merk value is the serialized Element,
        // not the raw value bytes.
        let real_element_bytes = Element::new_item(real_value.clone())
            .serialize(grove_version)
            .unwrap();
        let real_vh = grovedb_merk::tree::hash::value_hash(&real_element_bytes).unwrap();

        let fake_element_bytes = Element::new_item(fake_value.clone())
            .serialize(grove_version)
            .unwrap();

        // Decode the GroveDB proof, tamper with the leaf merk_proof, re-encode.
        let config = bincode::config::standard()
            .with_big_endian()
            .with_limit::<{ 256 * 1024 * 1024 }>();
        let (mut grovedb_proof, _): (GroveDBProof, _) =
            bincode::decode_from_slice(&proof_bytes, config).expect("should decode proof");

        // Navigate to the leaf layer's merk proof bytes.
        // For a V0 proof: root_layer -> lower_layers[TEST_LEAF] -> merk_proof
        // For a V1 proof: root_layer -> lower_layers[TEST_LEAF] -> merk_proof
        let tampered = match grovedb_proof {
            GroveDBProof::V0(ref mut v0) => {
                let leaf_layer = v0.root_layer.lower_layers.get_mut(TEST_LEAF).unwrap();
                tamper_kv_to_kvvaluehash(
                    &mut leaf_layer.merk_proof,
                    b"mykey",
                    &real_element_bytes,
                    &fake_element_bytes,
                    &real_vh,
                )
            }
            GroveDBProof::V1(ref mut v1) => {
                let leaf_layer = v1.root_layer.lower_layers.get_mut(TEST_LEAF).unwrap();
                match leaf_layer.merk_proof {
                    crate::operations::proof::ProofBytes::Merk(ref mut bytes) => {
                        tamper_kv_to_kvvaluehash(
                            bytes,
                            b"mykey",
                            &real_element_bytes,
                            &fake_element_bytes,
                            &real_vh,
                        )
                    }
                    _ => false,
                }
            }
        };
        assert!(tampered, "should have found and tampered the KV node");

        // Re-encode the tampered proof
        let tampered_proof_bytes =
            bincode::encode_to_vec(&grovedb_proof, config).expect("should re-encode proof");

        // === Verify the tampered proof ===
        // With the fix, this should fail with a value hash mismatch error.
        let tampered_result =
            GroveDb::verify_query_raw(&tampered_proof_bytes, &path_query, grove_version);
        assert!(
            tampered_result.is_err(),
            "tampered proof should be rejected, but got: {:?}",
            tampered_result.unwrap()
        );
        let err_msg = format!("{:?}", tampered_result.unwrap_err());
        assert!(
            err_msg.contains("must not contain an item element"),
            "error should mention item element rejection, got: {}",
            err_msg
        );
    }

    /// Replace a KV node (tag 0x03) in raw merk proof bytes with a KVValueHash
    /// node (tag 0x04) that has a different value but the same value_hash.
    ///
    /// Returns true if the substitution was made.
    fn tamper_kv_to_kvvaluehash(
        proof_bytes: &mut Vec<u8>,
        target_key: &[u8],
        real_element_bytes: &[u8],
        fake_element_bytes: &[u8],
        real_value_hash: &[u8; 32],
    ) -> bool {
        // Scan for the KV node pattern: [0x03, key_len, ...key, value_len_u16_be, ...value]
        let key_len = target_key.len() as u8;
        let value_len = real_element_bytes.len() as u16;

        // Build the expected KV encoding: [0x03, key_len, key..., value_len_be, value...]
        let mut expected = vec![0x03, key_len];
        expected.extend_from_slice(target_key);
        expected.extend_from_slice(&value_len.to_be_bytes());
        expected.extend_from_slice(real_element_bytes);

        // Find it in the proof bytes
        if let Some(pos) = proof_bytes
            .windows(expected.len())
            .position(|w| w == expected.as_slice())
        {
            // Build the replacement KVValueHash encoding:
            // [0x04, key_len, key..., fake_value_len_be, fake_value..., value_hash]
            let fake_value_len = fake_element_bytes.len() as u16;
            let mut replacement = vec![0x04, key_len];
            replacement.extend_from_slice(target_key);
            replacement.extend_from_slice(&fake_value_len.to_be_bytes());
            replacement.extend_from_slice(fake_element_bytes);
            replacement.extend_from_slice(real_value_hash);

            // Splice the replacement in
            proof_bytes.splice(pos..pos + expected.len(), replacement);
            true
        } else {
            false
        }
    }
}
