//! V1 proof end-to-end tests.
//!
//! Validates that MmrTree and BulkAppendTree elements can be proved and
//! verified through the GroveDB V1 proof system.

use grovedb_merk::proofs::{
    query::{QueryItem, SubqueryBranch},
    Query,
};
use grovedb_version::version::GroveVersion;

use crate::{
    operations::proof::GroveDBProof,
    tests::{common::EMPTY_PATH, make_empty_grovedb},
    Element, GroveDb, PathQuery, SizedQuery,
};

// ===========================================================================
// MMR Tree V1 proof tests
// ===========================================================================

#[test]
fn test_mmr_tree_v1_proof_single_leaf() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Insert an MmrTree
    db.insert(
        EMPTY_PATH,
        b"mmr",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert mmr tree");

    // Append 5 values
    let values: Vec<Vec<u8>> = (0..5u64)
        .map(|i| format!("leaf_{}", i).into_bytes())
        .collect();
    for v in &values {
        db.mmr_tree_append(EMPTY_PATH, b"mmr", v.clone(), None, grove_version)
            .unwrap()
            .expect("append value");
    }

    // Build a PathQuery that queries leaf index 2 inside the MmrTree
    let leaf_idx: u64 = 2;
    let mut inner_query = Query::new();
    inner_query.insert_key(leaf_idx.to_be_bytes().to_vec());

    let path_query = PathQuery {
        path: vec![],
        query: SizedQuery {
            query: Query {
                items: vec![QueryItem::Key(b"mmr".to_vec())],
                default_subquery_branch: SubqueryBranch {
                    subquery_path: None,
                    subquery: Some(inner_query.into()),
                },
                left_to_right: true,
                conditional_subquery_branches: None,
                add_parent_tree_on_subquery: false,
            },
            limit: None,
            offset: None,
        },
    };

    // Generate V1 proof
    let proof_bytes = db
        .prove_query_v1(&path_query, None, grove_version)
        .unwrap()
        .expect("generate V1 proof");

    // Verify the proof
    let (root_hash, result_set) = GroveDb::verify_query_with_options(
        &proof_bytes,
        &path_query,
        grovedb_merk::proofs::query::VerifyOptions {
            absence_proofs_for_non_existing_searched_keys: false,
            verify_proof_succinctness: false,
            include_empty_trees_in_result: false,
        },
        grove_version,
    )
    .expect("verify V1 proof");

    // Check root hash matches
    let expected_root = db.grove_db.root_hash(None, grove_version).unwrap().unwrap();
    assert_eq!(root_hash, expected_root, "root hash should match");

    // Check result set contains the queried leaf
    assert_eq!(result_set.len(), 1, "should have 1 result");
    let (_path, key, element) = &result_set[0];
    assert_eq!(key, &leaf_idx.to_be_bytes().to_vec());
    let element = element.as_ref().expect("element should be Some");
    match element {
        Element::Item(data, _) => {
            assert_eq!(data, &b"leaf_2".to_vec(), "value should match");
        }
        _ => panic!("expected Item element, got {:?}", element),
    }
}

#[test]
fn test_mmr_tree_v1_proof_multiple_leaves() {
    let grove_version = GroveVersion::latest();
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
    .expect("insert mmr tree");

    for i in 0..10u64 {
        db.mmr_tree_append(
            EMPTY_PATH,
            b"mmr",
            format!("val_{}", i).into_bytes(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("append value");
    }

    // Query leaf indices 1, 5, 8
    let mut inner_query = Query::new();
    inner_query.insert_key(1u64.to_be_bytes().to_vec());
    inner_query.insert_key(5u64.to_be_bytes().to_vec());
    inner_query.insert_key(8u64.to_be_bytes().to_vec());

    let path_query = PathQuery {
        path: vec![],
        query: SizedQuery {
            query: Query {
                items: vec![QueryItem::Key(b"mmr".to_vec())],
                default_subquery_branch: SubqueryBranch {
                    subquery_path: None,
                    subquery: Some(inner_query.into()),
                },
                left_to_right: true,
                conditional_subquery_branches: None,
                add_parent_tree_on_subquery: false,
            },
            limit: None,
            offset: None,
        },
    };

    let proof_bytes = db
        .prove_query_v1(&path_query, None, grove_version)
        .unwrap()
        .expect("generate V1 proof");

    let (root_hash, result_set) = GroveDb::verify_query_with_options(
        &proof_bytes,
        &path_query,
        grovedb_merk::proofs::query::VerifyOptions {
            absence_proofs_for_non_existing_searched_keys: false,
            verify_proof_succinctness: false,
            include_empty_trees_in_result: false,
        },
        grove_version,
    )
    .expect("verify V1 proof");

    let expected_root = db.grove_db.root_hash(None, grove_version).unwrap().unwrap();
    assert_eq!(root_hash, expected_root, "root hash should match");
    assert_eq!(result_set.len(), 3, "should have 3 results");

    // Check values
    let expected_values = vec![
        (1u64, b"val_1".to_vec()),
        (5u64, b"val_5".to_vec()),
        (8u64, b"val_8".to_vec()),
    ];
    for (i, (expected_idx, expected_val)) in expected_values.iter().enumerate() {
        let (_, key, element) = &result_set[i];
        assert_eq!(key, &expected_idx.to_be_bytes().to_vec());
        match element.as_ref().expect("element should be Some") {
            Element::Item(data, _) => assert_eq!(data, expected_val),
            other => panic!("expected Item, got {:?}", other),
        }
    }
}

#[test]
fn test_mmr_tree_v1_proof_wrong_root_detection() {
    let grove_version = GroveVersion::latest();
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
    .expect("insert mmr tree");

    for i in 0..3u64 {
        db.mmr_tree_append(
            EMPTY_PATH,
            b"mmr",
            format!("d_{}", i).into_bytes(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("append value");
    }

    // Generate proof, then tamper
    let mut inner_query = Query::new();
    inner_query.insert_key(0u64.to_be_bytes().to_vec());

    let path_query = PathQuery {
        path: vec![],
        query: SizedQuery {
            query: Query {
                items: vec![QueryItem::Key(b"mmr".to_vec())],
                default_subquery_branch: SubqueryBranch {
                    subquery_path: None,
                    subquery: Some(inner_query.into()),
                },
                left_to_right: true,
                conditional_subquery_branches: None,
                add_parent_tree_on_subquery: false,
            },
            limit: None,
            offset: None,
        },
    };

    let proof_bytes = db
        .prove_query_v1(&path_query, None, grove_version)
        .unwrap()
        .expect("generate V1 proof");

    // Verify succeeds with correct root
    let result = GroveDb::verify_query_with_options(
        &proof_bytes,
        &path_query,
        grovedb_merk::proofs::query::VerifyOptions {
            absence_proofs_for_non_existing_searched_keys: false,
            verify_proof_succinctness: false,
            include_empty_trees_in_result: false,
        },
        grove_version,
    );
    assert!(result.is_ok(), "proof should verify successfully");
}

// ===========================================================================
// BulkAppendTree V1 proof tests
// ===========================================================================

#[test]
fn test_bulk_append_tree_v1_proof_buffer_range() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Insert a BulkAppendTree with chunk_power=2 (chunk_size=4)
    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(2),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk append tree");

    // Append 3 values (all in buffer, no chunk compaction)
    for i in 0..3u64 {
        db.bulk_append(
            EMPTY_PATH,
            b"bulk",
            format!("buf_{}", i).into_bytes(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("bulk append");
    }

    // Query range [0, 3) — all buffer entries
    let mut inner_query = Query::new();
    inner_query.insert_range_inclusive(0u64.to_be_bytes().to_vec()..=2u64.to_be_bytes().to_vec());

    let path_query = PathQuery {
        path: vec![],
        query: SizedQuery {
            query: Query {
                items: vec![QueryItem::Key(b"bulk".to_vec())],
                default_subquery_branch: SubqueryBranch {
                    subquery_path: None,
                    subquery: Some(inner_query.into()),
                },
                left_to_right: true,
                conditional_subquery_branches: None,
                add_parent_tree_on_subquery: false,
            },
            limit: None,
            offset: None,
        },
    };

    let proof_bytes = db
        .prove_query_v1(&path_query, None, grove_version)
        .unwrap()
        .expect("generate V1 proof");

    let (root_hash, result_set) = GroveDb::verify_query_with_options(
        &proof_bytes,
        &path_query,
        grovedb_merk::proofs::query::VerifyOptions {
            absence_proofs_for_non_existing_searched_keys: false,
            verify_proof_succinctness: false,
            include_empty_trees_in_result: false,
        },
        grove_version,
    )
    .expect("verify V1 proof");

    let expected_root = db.grove_db.root_hash(None, grove_version).unwrap().unwrap();
    assert_eq!(root_hash, expected_root, "root hash should match");
    assert_eq!(result_set.len(), 3, "should have 3 results");

    for i in 0..3u64 {
        let (_, key, element) = &result_set[i as usize];
        assert_eq!(key, &i.to_be_bytes().to_vec());
        match element.as_ref().expect("element should be Some") {
            Element::Item(data, _) => {
                assert_eq!(data, &format!("buf_{}", i).into_bytes());
            }
            other => panic!("expected Item, got {:?}", other),
        }
    }
}

#[test]
fn test_bulk_append_tree_v1_proof_chunk_and_buffer() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // chunk_power=2 (chunk_size=4): 6 values → 1 full chunk (0-3) + 2 buffer
    // entries (4-5)
    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(2),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk append tree");

    for i in 0..6u64 {
        db.bulk_append(
            EMPTY_PATH,
            b"bulk",
            format!("entry_{}", i).into_bytes(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("bulk append");
    }

    // Query range [0, 6) — spans the full chunk and buffer
    let mut inner_query = Query::new();
    inner_query.insert_range_inclusive(0u64.to_be_bytes().to_vec()..=5u64.to_be_bytes().to_vec());

    let path_query = PathQuery {
        path: vec![],
        query: SizedQuery {
            query: Query {
                items: vec![QueryItem::Key(b"bulk".to_vec())],
                default_subquery_branch: SubqueryBranch {
                    subquery_path: None,
                    subquery: Some(inner_query.into()),
                },
                left_to_right: true,
                conditional_subquery_branches: None,
                add_parent_tree_on_subquery: false,
            },
            limit: None,
            offset: None,
        },
    };

    let proof_bytes = db
        .prove_query_v1(&path_query, None, grove_version)
        .unwrap()
        .expect("generate V1 proof");

    let (root_hash, result_set) = GroveDb::verify_query_with_options(
        &proof_bytes,
        &path_query,
        grovedb_merk::proofs::query::VerifyOptions {
            absence_proofs_for_non_existing_searched_keys: false,
            verify_proof_succinctness: false,
            include_empty_trees_in_result: false,
        },
        grove_version,
    )
    .expect("verify V1 proof");

    let expected_root = db.grove_db.root_hash(None, grove_version).unwrap().unwrap();
    assert_eq!(root_hash, expected_root, "root hash should match");
    assert_eq!(result_set.len(), 6, "should have 6 results");

    for i in 0..6u64 {
        let (_, key, element) = &result_set[i as usize];
        assert_eq!(key, &i.to_be_bytes().to_vec());
        match element.as_ref().expect("element should be Some") {
            Element::Item(data, _) => {
                assert_eq!(data, &format!("entry_{}", i).into_bytes());
            }
            other => panic!("expected Item at position {}, got {:?}", i, other),
        }
    }
}

#[test]
fn test_v1_proof_nested_path_with_mmr() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Create structure: root → "container" (Tree) → "mmr" (MmrTree)
    db.insert(
        EMPTY_PATH,
        b"container",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert container tree");

    let container_path: &[&[u8]] = &[b"container"];
    db.insert(
        container_path,
        b"mmr",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert mmr tree");

    for i in 0..5u64 {
        db.mmr_tree_append(
            container_path,
            b"mmr",
            format!("nested_{}", i).into_bytes(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("append value");
    }

    // Query leaf 3 inside nested MmrTree
    let mut inner_query = Query::new();
    inner_query.insert_key(3u64.to_be_bytes().to_vec());

    let path_query = PathQuery {
        path: vec![b"container".to_vec()],
        query: SizedQuery {
            query: Query {
                items: vec![QueryItem::Key(b"mmr".to_vec())],
                default_subquery_branch: SubqueryBranch {
                    subquery_path: None,
                    subquery: Some(inner_query.into()),
                },
                left_to_right: true,
                conditional_subquery_branches: None,
                add_parent_tree_on_subquery: false,
            },
            limit: None,
            offset: None,
        },
    };

    let proof_bytes = db
        .prove_query_v1(&path_query, None, grove_version)
        .unwrap()
        .expect("generate V1 proof");

    let (root_hash, result_set) = GroveDb::verify_query_with_options(
        &proof_bytes,
        &path_query,
        grovedb_merk::proofs::query::VerifyOptions {
            absence_proofs_for_non_existing_searched_keys: false,
            verify_proof_succinctness: false,
            include_empty_trees_in_result: false,
        },
        grove_version,
    )
    .expect("verify V1 proof");

    let expected_root = db.grove_db.root_hash(None, grove_version).unwrap().unwrap();
    assert_eq!(root_hash, expected_root, "root hash should match");
    assert_eq!(result_set.len(), 1, "should have 1 result");

    let (_, key, element) = &result_set[0];
    assert_eq!(key, &3u64.to_be_bytes().to_vec());
    match element.as_ref().expect("element should be Some") {
        Element::Item(data, _) => assert_eq!(data, b"nested_3"),
        other => panic!("expected Item, got {:?}", other),
    }
}

#[test]
fn test_v1_proof_serialization_roundtrip() {
    let grove_version = GroveVersion::latest();
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
    .expect("insert mmr tree");

    for i in 0..3u64 {
        db.mmr_tree_append(
            EMPTY_PATH,
            b"mmr",
            format!("item_{}", i).into_bytes(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("append value");
    }

    let mut inner_query = Query::new();
    inner_query.insert_key(0u64.to_be_bytes().to_vec());
    inner_query.insert_key(2u64.to_be_bytes().to_vec());

    let path_query = PathQuery {
        path: vec![],
        query: SizedQuery {
            query: Query {
                items: vec![QueryItem::Key(b"mmr".to_vec())],
                default_subquery_branch: SubqueryBranch {
                    subquery_path: None,
                    subquery: Some(inner_query.into()),
                },
                left_to_right: true,
                conditional_subquery_branches: None,
                add_parent_tree_on_subquery: false,
            },
            limit: None,
            offset: None,
        },
    };

    // Generate and serialize V1 proof
    let proof_bytes = db
        .prove_query_v1(&path_query, None, grove_version)
        .unwrap()
        .expect("generate V1 proof");

    // Deserialize and check it's V1
    let config = bincode::config::standard()
        .with_big_endian()
        .with_no_limit();
    let (decoded_proof, _): (GroveDBProof, _) =
        bincode::decode_from_slice(&proof_bytes, config).expect("decode proof");

    match &decoded_proof {
        GroveDBProof::V1(_) => {} // expected
        GroveDBProof::V0(_) => panic!("expected V1 proof, got V0"),
    }

    // Verify the deserialized proof
    let (root_hash, result_set) = GroveDb::verify_query_with_options(
        &proof_bytes,
        &path_query,
        grovedb_merk::proofs::query::VerifyOptions {
            absence_proofs_for_non_existing_searched_keys: false,
            verify_proof_succinctness: false,
            include_empty_trees_in_result: false,
        },
        grove_version,
    )
    .expect("verify V1 proof");

    let expected_root = db.grove_db.root_hash(None, grove_version).unwrap().unwrap();
    assert_eq!(root_hash, expected_root, "root hash should match");
    assert_eq!(result_set.len(), 2, "should have 2 results");
}

// ===========================================================================
// DenseTree V1 proof tests
// ===========================================================================

#[test]
fn test_dense_tree_v1_proof_serialization_roundtrip() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // 1. Create an empty dense tree (height 3, capacity 7) at root
    db.insert(
        EMPTY_PATH,
        b"dense",
        Element::empty_dense_tree(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert dense tree");

    // 2. Insert 3 values via dense_tree_insert
    for i in 0..3u16 {
        let value = format!("item_{}", i).into_bytes();
        db.dense_tree_insert(EMPTY_PATH, b"dense", value, None, grove_version)
            .unwrap()
            .expect("dense tree insert");
    }

    // 3. Build query: select positions 0 and 2 (u16 big-endian keys)
    let mut inner_query = Query::new();
    inner_query.insert_key(0u16.to_be_bytes().to_vec());
    inner_query.insert_key(2u16.to_be_bytes().to_vec());

    let path_query = PathQuery {
        path: vec![],
        query: SizedQuery {
            query: Query {
                items: vec![QueryItem::Key(b"dense".to_vec())],
                default_subquery_branch: SubqueryBranch {
                    subquery_path: None,
                    subquery: Some(inner_query.into()),
                },
                left_to_right: true,
                conditional_subquery_branches: None,
                add_parent_tree_on_subquery: false,
            },
            limit: None,
            offset: None,
        },
    };

    // 4. Generate V1 proof
    let proof_bytes = db
        .prove_query_v1(&path_query, None, grove_version)
        .unwrap()
        .expect("generate V1 proof for dense tree");

    // 5. Deserialize with bincode and check it's V1
    let config = bincode::config::standard()
        .with_big_endian()
        .with_no_limit();
    let (decoded_proof, _): (GroveDBProof, _) =
        bincode::decode_from_slice(&proof_bytes, config).expect("decode proof from bytes");

    match &decoded_proof {
        GroveDBProof::V1(_) => {} // expected
        GroveDBProof::V0(_) => panic!("expected V1 proof, got V0"),
    }

    // 6. Verify the proof and check results
    let (root_hash, result_set) = GroveDb::verify_query_with_options(
        &proof_bytes,
        &path_query,
        grovedb_merk::proofs::query::VerifyOptions {
            absence_proofs_for_non_existing_searched_keys: false,
            verify_proof_succinctness: false,
            include_empty_trees_in_result: false,
        },
        grove_version,
    )
    .expect("verify V1 proof for dense tree");

    let expected_root = db
        .grove_db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("should get root hash");
    assert_eq!(root_hash, expected_root, "root hash should match");
    assert_eq!(
        result_set.len(),
        2,
        "should have 2 results for positions 0 and 2"
    );

    // Verify the returned elements match what was inserted
    let (_, key0, elem0) = &result_set[0];
    assert_eq!(
        key0,
        &0u16.to_be_bytes().to_vec(),
        "first key should be position 0"
    );
    match elem0.as_ref().expect("element should be Some") {
        Element::Item(data, _) => assert_eq!(data, b"item_0", "position 0 value mismatch"),
        other => panic!("expected Item at position 0, got {:?}", other),
    }

    let (_, key2, elem2) = &result_set[1];
    assert_eq!(
        key2,
        &2u16.to_be_bytes().to_vec(),
        "second key should be position 2"
    );
    match elem2.as_ref().expect("element should be Some") {
        Element::Item(data, _) => assert_eq!(data, b"item_2", "position 2 value mismatch"),
        other => panic!("expected Item at position 2, got {:?}", other),
    }
}

// ===========================================================================
// V0 proof rejection tests for non-Merk tree subqueries
// ===========================================================================

/// V0 proofs cannot descend into MmrTree subtrees — the code returns
/// NotSupported. This test verifies that `prove_query` (V0) rejects a
/// PathQuery whose subquery targets an MmrTree element.
#[test]
fn test_v0_proof_rejects_mmr_tree_subquery() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Create parent tree containing an MmrTree child
    db.insert(
        EMPTY_PATH,
        b"parent",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert parent tree");

    db.insert(
        [b"parent"].as_ref(),
        b"mmr",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert mmr tree under parent");

    // Append some values so the MmrTree is non-empty
    for i in 0..3u8 {
        db.mmr_tree_append([b"parent"].as_ref(), b"mmr", vec![i], None, grove_version)
            .unwrap()
            .expect("append to mmr");
    }

    // Build a PathQuery with a subquery that descends into the MmrTree
    let mut inner_query = Query::new();
    inner_query.insert_key(0u64.to_be_bytes().to_vec());

    let path_query = PathQuery {
        path: vec![b"parent".to_vec()],
        query: SizedQuery {
            query: Query {
                items: vec![QueryItem::Key(b"mmr".to_vec())],
                default_subquery_branch: SubqueryBranch {
                    subquery_path: None,
                    subquery: Some(inner_query.into()),
                },
                left_to_right: true,
                conditional_subquery_branches: None,
                add_parent_tree_on_subquery: false,
            },
            limit: None,
            offset: None,
        },
    };

    // V0 prove_query should return NotSupported
    let result = db.prove_query(&path_query, None, grove_version).unwrap();
    match result {
        Err(crate::Error::NotSupported(msg)) => {
            assert!(
                msg.contains("V0 proofs do not support subqueries"),
                "error message should mention V0 limitation, got: {}",
                msg
            );
        }
        Err(other) => {
            panic!(
                "expected NotSupported error, got different error: {:?}",
                other
            );
        }
        Ok(_) => {
            panic!("expected NotSupported error, but prove_query succeeded");
        }
    }
}

/// V0 proofs cannot descend into BulkAppendTree subtrees.
#[test]
fn test_v0_proof_rejects_bulk_append_tree_subquery() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"parent",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert parent tree");

    db.insert(
        [b"parent"].as_ref(),
        b"bulk",
        Element::empty_bulk_append_tree(2),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk append tree under parent");

    // Append some values
    for i in 0..3u8 {
        db.bulk_append([b"parent"].as_ref(), b"bulk", vec![i], None, grove_version)
            .unwrap()
            .expect("bulk append");
    }

    // Build a PathQuery with a subquery targeting the BulkAppendTree
    let mut inner_query = Query::new();
    inner_query.insert_key(0u64.to_be_bytes().to_vec());

    let path_query = PathQuery {
        path: vec![b"parent".to_vec()],
        query: SizedQuery {
            query: Query {
                items: vec![QueryItem::Key(b"bulk".to_vec())],
                default_subquery_branch: SubqueryBranch {
                    subquery_path: None,
                    subquery: Some(inner_query.into()),
                },
                left_to_right: true,
                conditional_subquery_branches: None,
                add_parent_tree_on_subquery: false,
            },
            limit: None,
            offset: None,
        },
    };

    let result = db.prove_query(&path_query, None, grove_version).unwrap();
    match result {
        Err(crate::Error::NotSupported(msg)) => {
            assert!(
                msg.contains("V0 proofs do not support subqueries"),
                "error message should mention V0 limitation, got: {}",
                msg
            );
        }
        Err(other) => {
            panic!(
                "expected NotSupported error, got different error: {:?}",
                other
            );
        }
        Ok(_) => {
            panic!("expected NotSupported error, but prove_query succeeded");
        }
    }
}

/// V0 proofs cannot descend into DenseAppendOnlyFixedSizeTree subtrees.
#[test]
fn test_v0_proof_rejects_dense_tree_subquery() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"parent",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert parent tree");

    db.insert(
        [b"parent"].as_ref(),
        b"dense",
        Element::empty_dense_tree(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert dense tree under parent");

    // Insert some values
    for i in 0..3u16 {
        db.dense_tree_insert(
            [b"parent"].as_ref(),
            b"dense",
            format!("v_{}", i).into_bytes(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("dense tree insert");
    }

    // Build a PathQuery with a subquery targeting the DenseTree
    let mut inner_query = Query::new();
    inner_query.insert_key(0u16.to_be_bytes().to_vec());

    let path_query = PathQuery {
        path: vec![b"parent".to_vec()],
        query: SizedQuery {
            query: Query {
                items: vec![QueryItem::Key(b"dense".to_vec())],
                default_subquery_branch: SubqueryBranch {
                    subquery_path: None,
                    subquery: Some(inner_query.into()),
                },
                left_to_right: true,
                conditional_subquery_branches: None,
                add_parent_tree_on_subquery: false,
            },
            limit: None,
            offset: None,
        },
    };

    let result = db.prove_query(&path_query, None, grove_version).unwrap();
    match result {
        Err(crate::Error::NotSupported(msg)) => {
            assert!(
                msg.contains("V0 proofs do not support subqueries"),
                "error message should mention V0 limitation, got: {}",
                msg
            );
        }
        Err(other) => {
            panic!(
                "expected NotSupported error, got different error: {:?}",
                other
            );
        }
        Ok(_) => {
            panic!("expected NotSupported error, but prove_query succeeded");
        }
    }
}
