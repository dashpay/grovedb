//! Proof succinctness and completeness tests.
//!
//! Succinctness: a proof should not contain extra data beyond what the query
//! requires. `verify_query` (succinctness=true) should reject such proofs,
//! while `verify_subset_query` (succinctness=false) should accept them.
//!
//! Completeness: a proof must not omit lower layers for non-empty trees that
//! the query traverses into. Both verify methods must reject such proofs.

use grovedb_version::version::GroveVersion;

use crate::{
    operations::proof::GroveDBProof,
    tests::{make_deep_tree, TEST_LEAF},
    GroveDb, PathQuery, Query,
};

/// Test succinctness enforcement.
///
/// Tree structure (from make_deep_tree):
///   root -> test_leaf -> innertree  -> {k1, k2, k3}
///                     -> innertree4 -> {k4, k5}
///
/// Broad query: all items under test_leaf/* (both subtrees)
/// Narrow query: only items under test_leaf/innertree (one subtree)
///
/// Generate proof for the broad query, then verify with the narrow query.
/// - verify_subset_query (succinctness=false): should PASS (extra data OK)
/// - verify_query (succinctness=true): should FAIL (extra data not OK)
#[test]
fn test_succinctness_rejects_extra_proof_data() {
    let grove_version = GroveVersion::latest();
    let db = make_deep_tree(grove_version);

    // Broad query: all subtrees under TEST_LEAF
    let mut broad_inner = Query::new();
    broad_inner.insert_all();
    let mut broad_outer = Query::new();
    broad_outer.insert_all();
    broad_outer.set_subquery(broad_inner);
    let broad_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], broad_outer);

    // Generate proof covering both innertree and innertree4
    let proof_bytes = db
        .prove_query(&broad_query, None, grove_version)
        .unwrap()
        .expect("should generate broad proof");

    // Sanity: broad proof verifies with broad query
    let (root_hash, broad_results) =
        GroveDb::verify_query(&proof_bytes, &broad_query, grove_version)
            .expect("broad proof should verify with broad query");
    let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
    assert_eq!(root_hash, expected_root);
    assert_eq!(broad_results.len(), 5, "broad query should return 5 items");

    // Narrow query: only innertree (not innertree4)
    let mut narrow_inner = Query::new();
    narrow_inner.insert_all();
    let mut narrow_outer = Query::new();
    narrow_outer.insert_key(b"innertree".to_vec());
    narrow_outer.set_subquery(narrow_inner);
    let narrow_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], narrow_outer);

    // verify_subset_query (succinctness OFF): should accept the broad proof
    // with the narrow query — extra data (innertree4) is tolerated
    let subset_result = GroveDb::verify_subset_query(&proof_bytes, &narrow_query, grove_version);
    assert!(
        subset_result.is_ok(),
        "verify_subset_query should accept broad proof with narrow query"
    );
    let (subset_root, subset_results) = subset_result.unwrap();
    assert_eq!(subset_root, expected_root);
    assert_eq!(
        subset_results.len(),
        3,
        "subset verification should return only the 3 items from innertree"
    );

    // verify_query (succinctness ON): should reject the broad proof because
    // it contains extra data (innertree4 proof) not required by the narrow query
    let strict_result = GroveDb::verify_query(&proof_bytes, &narrow_query, grove_version);
    assert!(
        strict_result.is_err(),
        "verify_query should reject broad proof verified with narrow query (extra data)"
    );
}

/// Test completeness enforcement.
///
/// Stripping a non-empty subtree's lower-layer proof must be rejected by
/// both verify_query and verify_subset_query — this is a soundness
/// requirement, not a succinctness preference.
#[test]
fn test_missing_lower_layer_for_non_empty_tree_is_rejected() {
    let grove_version = GroveVersion::latest();
    let db = make_deep_tree(grove_version);

    // Query all items under all subtrees of TEST_LEAF
    let mut inner_query = Query::new();
    inner_query.insert_all();
    let mut outer_query = Query::new();
    outer_query.insert_all();
    outer_query.set_subquery(inner_query);
    let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], outer_query);

    // Generate and verify honest proof
    let proof_bytes = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .expect("should generate proof");

    let (honest_root_hash, honest_results) =
        GroveDb::verify_query(&proof_bytes, &path_query, grove_version)
            .expect("honest proof should verify");

    let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
    assert_eq!(honest_root_hash, expected_root);
    assert_eq!(
        honest_results.len(),
        5,
        "honest proof should return 5 items (k1..k5)"
    );

    // Tamper: decode proof, remove innertree4's lower layer, re-encode
    let config = bincode::config::standard()
        .with_big_endian()
        .with_no_limit();
    let mut grovedb_proof: GroveDBProof = bincode::decode_from_slice(&proof_bytes, config)
        .expect("should decode proof")
        .0;

    let test_leaf_key = TEST_LEAF.to_vec();
    let innertree4_key = b"innertree4".to_vec();
    let had_layer = match &mut grovedb_proof {
        GroveDBProof::V0(v0) => v0
            .root_layer
            .lower_layers
            .get_mut(&test_leaf_key)
            .and_then(|tl| tl.lower_layers.remove(&innertree4_key))
            .is_some(),
        GroveDBProof::V1(v1) => v1
            .root_layer
            .lower_layers
            .get_mut(&test_leaf_key)
            .and_then(|tl| tl.lower_layers.remove(&innertree4_key))
            .is_some(),
    };
    assert!(had_layer, "innertree4 should have had a lower layer proof");

    let tampered_bytes =
        bincode::encode_to_vec(&grovedb_proof, config).expect("should re-encode tampered proof");

    // Both must reject — this is a completeness/soundness requirement
    let result = GroveDb::verify_query(&tampered_bytes, &path_query, grove_version);
    assert!(
        result.is_err(),
        "verify_query must reject proof missing a non-empty subtree's lower layer"
    );

    let result = GroveDb::verify_subset_query(&tampered_bytes, &path_query, grove_version);
    assert!(
        result.is_err(),
        "verify_subset_query must reject proof missing a non-empty subtree's lower layer"
    );
}
