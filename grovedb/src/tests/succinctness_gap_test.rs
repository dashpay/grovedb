//! Proof succinctness and completeness tests.
//!
//! Succinctness: a proof should not contain extra data beyond what the query
//! requires. `verify_query` (succinctness=true) should reject such proofs,
//! while `verify_subset_query` (succinctness=false) should accept them.
//!
//! Completeness: a proof must not omit lower layers for non-empty trees that
//! the query traverses into. Both verify methods must reject such proofs.

use grovedb_version::version::{v1::GROVE_V1, GroveVersion};

use crate::{
    operations::proof::{GroveDBProof, LayerProof, ProofBytes},
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

/// Same as test_succinctness_rejects_extra_proof_data but forces V0 proofs
/// (using GROVE_V1 which generates MerkOnlyLayerProof).
#[test]
fn test_succinctness_rejects_extra_proof_data_v0() {
    let grove_version = &GROVE_V1;
    let db = make_deep_tree(grove_version);

    let mut broad_inner = Query::new();
    broad_inner.insert_all();
    let mut broad_outer = Query::new();
    broad_outer.insert_all();
    broad_outer.set_subquery(broad_inner);
    let broad_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], broad_outer);

    let proof_bytes = db
        .prove_query(&broad_query, None, grove_version)
        .unwrap()
        .expect("should generate broad proof");

    let mut narrow_inner = Query::new();
    narrow_inner.insert_all();
    let mut narrow_outer = Query::new();
    narrow_outer.insert_key(b"innertree".to_vec());
    narrow_outer.set_subquery(narrow_inner);
    let narrow_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], narrow_outer);

    let subset_result = GroveDb::verify_subset_query(&proof_bytes, &narrow_query, grove_version);
    assert!(
        subset_result.is_ok(),
        "V0: verify_subset_query should accept broad proof with narrow query"
    );

    let strict_result = GroveDb::verify_query(&proof_bytes, &narrow_query, grove_version);
    assert!(
        strict_result.is_err(),
        "V0: verify_query should reject broad proof with narrow query (extra data)"
    );
}

/// Same as test_missing_lower_layer but forces V0 proofs.
#[test]
fn test_missing_lower_layer_for_non_empty_tree_is_rejected_v0() {
    let grove_version = &GROVE_V1;
    let db = make_deep_tree(grove_version);

    let mut inner_query = Query::new();
    inner_query.insert_all();
    let mut outer_query = Query::new();
    outer_query.insert_all();
    outer_query.set_subquery(inner_query);
    let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], outer_query);

    let proof_bytes = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .expect("should generate proof");

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

    let result = GroveDb::verify_query(&tampered_bytes, &path_query, grove_version);
    assert!(
        result.is_err(),
        "V0: verify_query must reject proof missing a non-empty subtree's lower layer"
    );

    let result = GroveDb::verify_subset_query(&tampered_bytes, &path_query, grove_version);
    assert!(
        result.is_err(),
        "V0: verify_subset_query must reject proof missing a non-empty subtree's lower layer"
    );
}

/// Regression: a subset verification whose query stops at a tree ELEMENT must
/// still verify against a proof that descended INTO that tree.
///
/// This is the ordinary shape of `verify_subset_query`: a wide proof is
/// generated once, then re-verified against a narrower query. Dash Platform
/// reads a shielded `CommitmentTree`'s total note count exactly this way —
/// single-key query, no subquery, against the note-fetch proof it already
/// holds.
///
/// The tree element is the only result, and it must come back bound: the
/// verifier derives the lower layer's root and checks
/// `combine_hash(H(value), child_root)` against the parent-committed value
/// hash, without reporting any of the lower layer's rows.
#[test]
fn test_subset_query_for_tree_element_itself_against_descending_proof() {
    let grove_version = GroveVersion::latest();
    let db = make_deep_tree(grove_version);
    let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();

    // Wide proof: descends into innertree, so the proof carries a lower layer
    // at that key.
    let mut inner = Query::new();
    inner.insert_all();
    let mut outer = Query::new();
    outer.insert_key(b"innertree".to_vec());
    outer.set_subquery(inner);
    let broad_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], outer);

    let proof_bytes = db
        .prove_query(&broad_query, None, grove_version)
        .unwrap()
        .expect("should generate descending proof");

    // Narrow query: the innertree element itself, no subquery.
    let narrow_query = PathQuery::new_single_key(vec![TEST_LEAF.to_vec()], b"innertree".to_vec());

    let (subset_root, subset_results) =
        GroveDb::verify_subset_query(&proof_bytes, &narrow_query, grove_version)
            .expect("subset verification must accept a query that stops at the tree element");

    assert_eq!(subset_root, expected_root);
    assert_eq!(
        subset_results.len(),
        1,
        "only the tree element itself is a result; the lower layer's rows are not reported"
    );
    let (path, key, element) = &subset_results[0];
    assert_eq!(
        path,
        &vec![TEST_LEAF.to_vec()],
        "the tree must be reported under its PARENT path, so (path, key) lookups hit"
    );
    assert_eq!(key, b"innertree");
    assert!(
        element
            .as_ref()
            .expect("element present")
            .is_non_empty_merk_tree(),
        "the reported element must be the innertree subtree itself"
    );

    // Succinct mode still refuses: for this narrow query the lower layer is
    // data the query never asked for.
    assert!(
        GroveDb::verify_query(&proof_bytes, &narrow_query, grove_version).is_err(),
        "verify_query must still reject a proof carrying an unrequested lower layer"
    );
}

/// The element bytes reported by the subset path above stay BOUND.
///
/// A `KVValueHash`-family node hashes only `(key, value_hash)`, so a prover
/// that could get a tree element reported without any child-hash check could
/// swap in forged element bytes under a genuine root hash. Two tampers prove
/// the binding is load-bearing rather than incidental:
///
///   1. a dummy lower layer attached where none belongs, and
///   2. a real-but-wrong lower layer (a sibling subtree's proof).
///
/// Both must be rejected even though succinctness checking is OFF.
#[test]
fn test_subset_mode_still_binds_element_bytes_to_lower_layer() {
    let grove_version = GroveVersion::latest();
    let db = make_deep_tree(grove_version);
    let config = bincode::config::standard()
        .with_big_endian()
        .with_no_limit();

    // Wide proof descending into BOTH innertree and innertree4, so the two
    // sibling lower layers are available to swap.
    let mut inner = Query::new();
    inner.insert_all();
    let mut outer = Query::new();
    outer.insert_all();
    outer.set_subquery(inner);
    let broad_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], outer);
    let proof_bytes = db
        .prove_query(&broad_query, None, grove_version)
        .unwrap()
        .expect("should generate descending proof");

    let narrow_query = PathQuery::new_single_key(vec![TEST_LEAF.to_vec()], b"innertree".to_vec());

    // Sanity: untampered, the narrow subset query verifies.
    GroveDb::verify_subset_query(&proof_bytes, &narrow_query, grove_version)
        .expect("honest proof must verify with the narrow query");

    let decode = |bytes: &[u8]| -> GroveDBProof {
        bincode::decode_from_slice(bytes, config)
            .expect("should decode proof")
            .0
    };
    let test_leaf_key = TEST_LEAF.to_vec();
    let innertree_key = b"innertree".to_vec();
    let innertree4_key = b"innertree4".to_vec();

    // These proofs are V1 envelopes; only V1 carries the typed lower-layer
    // proof bytes this test tampers with.
    fn root_layer_mut(proof: &mut GroveDBProof) -> &mut LayerProof {
        match proof {
            GroveDBProof::V1(v1) => &mut v1.root_layer,
            GroveDBProof::V0(_) => panic!("expected a V1 proof envelope"),
        }
    }

    // Tamper 1: replace innertree's lower layer with an empty dummy.
    let mut dummy = decode(&proof_bytes);
    root_layer_mut(&mut dummy)
        .lower_layers
        .get_mut(&test_leaf_key)
        .expect("TEST_LEAF layer")
        .lower_layers
        .insert(
            innertree_key.clone(),
            LayerProof {
                merk_proof: ProofBytes::Merk(Vec::new()),
                lower_layers: Default::default(),
            },
        );
    let dummy_bytes = bincode::encode_to_vec(&dummy, config).expect("re-encode");
    assert!(
        GroveDb::verify_subset_query(&dummy_bytes, &narrow_query, grove_version).is_err(),
        "a dummy lower layer must not let unbound element bytes through in subset mode"
    );

    // Tamper 2: give innertree its SIBLING's lower layer — a structurally
    // valid Merk proof that commits to a different root.
    let mut swapped = decode(&proof_bytes);
    let leaf_layers = &mut root_layer_mut(&mut swapped)
        .lower_layers
        .get_mut(&test_leaf_key)
        .expect("TEST_LEAF layer")
        .lower_layers;
    let sibling = leaf_layers
        .get(&innertree4_key)
        .expect("innertree4 lower layer")
        .clone();
    leaf_layers.insert(innertree_key, sibling);
    let swapped_bytes = bincode::encode_to_vec(&swapped, config).expect("re-encode");
    assert!(
        GroveDb::verify_subset_query(&swapped_bytes, &narrow_query, grove_version).is_err(),
        "the reported element must be bound to ITS OWN child root, not any valid subtree proof"
    );
}
