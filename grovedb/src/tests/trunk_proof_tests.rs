//! Trunk proof tests

#[cfg(test)]
mod tests {
    use blake3::Hasher;
    use grovedb_merk::proofs::{
        branch::depth::calculate_max_tree_depth_from_count, encode_into, Decoder, Node, Op,
    };
    use grovedb_version::version::GroveVersion;
    use rand::{rngs::StdRng, RngExt, SeedableRng};

    use crate::{
        operations::proof::{GroveDBProof, ProofBytes},
        query::PathTrunkChunkQuery,
        tests::{common::EMPTY_PATH, make_empty_grovedb},
        Element, GroveDb,
    };

    #[test]
    fn test_trunk_proof_with_count_sum_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Use a seeded RNG for reproducibility
        let mut rng = StdRng::seed_from_u64(12345);

        // Insert 3 trees at the root level
        // Tree 1: regular tree
        db.insert(
            EMPTY_PATH,
            b"tree1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful tree1 insert");

        // Tree 2: another regular tree
        db.insert(
            EMPTY_PATH,
            b"tree2",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful tree2 insert");

        // Tree 3: CountSumTree - this is where we'll add our items
        db.insert(
            EMPTY_PATH,
            b"count_sum_tree",
            Element::empty_count_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful count_sum_tree insert");

        // Insert 100 SumItems into the CountSumTree
        // Keys are random numbers 0-10 (as bytes), values are random sums 0-10
        for i in 0u32..100 {
            let key_num: u8 = rng.random_range(0..=10);
            let sum_value: i64 = rng.random_range(0..=10);

            // Create a unique key by combining the random number with the index
            let mut key = vec![key_num];
            key.extend_from_slice(&i.to_be_bytes());

            db.insert(
                &[b"count_sum_tree"],
                &key,
                Element::new_sum_item(sum_value),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("successful sum_item insert");
        }

        // Now test the trunk proof
        // Use max_depth=4 to test chunking (tree_depth is ~7 for 100 elements)
        let query = PathTrunkChunkQuery::new(vec![b"count_sum_tree".to_vec()], 4);

        // Generate the trunk proof
        let proof = db
            .prove_trunk_chunk(&query, grove_version)
            .unwrap()
            .expect("successful trunk proof generation");

        // Verify the trunk proof
        let (root_hash, result) = GroveDb::verify_trunk_chunk_proof(&proof, &query, grove_version)
            .expect("successful trunk proof verification");

        // Verify we got a valid root hash
        assert_ne!(root_hash, [0u8; 32], "root hash should not be all zeros");

        // Verify we got elements back
        assert!(!result.elements.is_empty(), "should have elements");

        // Verify chunk_depths is calculated correctly
        // tree_depth=9 with max_depth=4 should give [3, 3, 3]
        // (100 elements: N(9)=88 <= 100 < 143=N(10), so max height = 9)
        assert_eq!(
            result.max_tree_depth, 9,
            "tree depth should be 9 for 100 elements"
        );
        assert_eq!(
            result.chunk_depths,
            vec![3, 3, 3],
            "chunk depths should be [3, 3, 3] for tree_depth=9, max_depth=4"
        );

        // Verify we have the expected number of elements in the first chunk
        // First chunk has 3 levels, which should have up to 2^3-1=7 nodes
        assert!(
            result.elements.len() >= 4 && result.elements.len() <= 7,
            "should have 4-7 elements in first 3 levels, got {}",
            result.elements.len()
        );

        // Verify we have leaf keys (nodes at the truncation boundary)
        // These are keys whose children are Hash nodes
        assert!(
            !result.leaf_keys.is_empty(),
            "should have leaf keys for truncated tree"
        );

        // All elements should be SumItems
        for (key, element) in &result.elements {
            assert!(
                matches!(element, Element::SumItem(..)),
                "element at key {:?} should be SumItem, got {:?}",
                key,
                element
            );
        }

        // Verify that the proof is V1 and the lowest layer proof contains
        // only KV and Hash nodes (correct node types for GroveDB elements)
        let config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        let decoded_proof: GroveDBProof = bincode::decode_from_slice(&proof, config)
            .expect("should decode proof")
            .0;

        let GroveDBProof::V1(proof_v1) = decoded_proof else {
            panic!("expected V1 proof from latest version");
        };

        // Get the lowest layer proof (the count_sum_tree merk proof)
        let lowest_layer = proof_v1
            .root_layer
            .lower_layers
            .get(b"count_sum_tree".as_slice())
            .expect("should have count_sum_tree layer");

        let merk_bytes = match &lowest_layer.merk_proof {
            ProofBytes::Merk(bytes) => bytes,
            other => panic!(
                "expected Merk proof bytes, got {:?}",
                std::mem::discriminant(other)
            ),
        };

        // Decode and check the merk proof ops
        let ops: Vec<Op> = Decoder::new(merk_bytes)
            .collect::<Result<Vec<_>, _>>()
            .expect("should decode merk proof");

        let mut kv_count = 0;
        let mut hash_count = 0;
        for op in &ops {
            if let Op::Push(node) = op {
                match node {
                    Node::KV(..) => kv_count += 1,
                    Node::Hash(..) => hash_count += 1,
                    other => panic!(
                        "Expected only KV or Hash nodes in trunk proof for CountSumTree with \
                         SumItems, but found {:?}. This indicates create_chunk is not using \
                         correct node types.",
                        other
                    ),
                }
            }
        }

        // Verify we have the expected KV nodes (elements) and Hash nodes (truncated
        // children) With first_chunk_depth=3: 2^3-1=7 KV nodes, 2^3=8 Hash
        // nodes
        assert_eq!(
            kv_count, 7,
            "should have 7 KV nodes for SumItems in CountSumTree (depth 3)"
        );
        assert_eq!(
            hash_count, 8,
            "should have 8 Hash nodes for truncated children at depth boundary"
        );
    }

    #[test]
    fn test_trunk_proof_full_tree_no_truncation() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Use a seeded RNG for reproducibility
        let mut rng = StdRng::seed_from_u64(12345);

        // Insert CountSumTree at root
        db.insert(
            EMPTY_PATH,
            b"count_sum_tree",
            Element::empty_count_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful count_sum_tree insert");

        // Insert 100 ItemWithSumItems into the CountSumTree
        // Use random 32-byte keys (hash of index)
        for i in 0u32..100 {
            let mut hasher = Hasher::new();
            hasher.update(&i.to_be_bytes());
            let key: [u8; 32] = *hasher.finalize().as_bytes();
            let sum_value: i64 = rng.random_range(0..=10);
            let item_value: Vec<u8> = vec![i as u8; 10];

            db.insert(
                &[b"count_sum_tree"],
                &key,
                Element::new_item_with_sum_item(item_value, sum_value),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("successful item_with_sum insert");
        }

        // Use max_depth equal to the max AVL height for 100 elements
        // This should return all elements with no truncation
        let max_depth = calculate_max_tree_depth_from_count(100);
        let query = PathTrunkChunkQuery::new(vec![b"count_sum_tree".to_vec()], max_depth);

        // Generate the trunk proof
        let proof = db
            .prove_trunk_chunk(&query, grove_version)
            .unwrap()
            .expect("successful trunk proof generation");

        // Verify the trunk proof
        let (root_hash, result) = GroveDb::verify_trunk_chunk_proof(&proof, &query, grove_version)
            .expect("successful trunk proof verification");

        // Verify we got a valid root hash
        assert_ne!(root_hash, [0u8; 32], "root hash should not be all zeros");

        // With max_depth = max AVL height, we should get all 100 elements
        assert_eq!(
            result.elements.len(),
            100,
            "should have all 100 elements when max_depth >= tree_depth"
        );

        // tree_depth should match the calculated max AVL height
        assert_eq!(
            result.max_tree_depth, max_depth,
            "tree depth should match calculated max AVL height for 100 elements"
        );
        assert_eq!(
            result.chunk_depths,
            vec![max_depth],
            "chunk depths should be [max_depth] when max_depth == tree_depth"
        );

        // No leaf keys since there's no truncation
        assert!(
            result.leaf_keys.is_empty(),
            "should have no leaf keys when entire tree is returned"
        );

        // All elements should be ItemWithSumItem
        for (key, element) in &result.elements {
            assert!(
                matches!(element, Element::ItemWithSumItem(..)),
                "element at key {:?} should be ItemWithSumItem, got {:?}",
                key,
                element
            );
        }
    }

    #[test]
    fn test_trunk_proof_full_tree_some_truncation() {
        use grovedb_merk::proofs::branch::depth::calculate_chunk_depths;

        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Use a seeded RNG for reproducibility
        let mut rng = StdRng::seed_from_u64(12345);

        // Insert CountSumTree at root
        db.insert(
            EMPTY_PATH,
            b"count_sum_tree",
            Element::empty_count_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful count_sum_tree insert");

        // Insert 100 ItemWithSumItems into the CountSumTree
        // Use random 32-byte keys (hash of index)
        for i in 0u32..100 {
            let mut hasher = Hasher::new();
            hasher.update(&i.to_be_bytes());
            let key: [u8; 32] = *hasher.finalize().as_bytes();
            let sum_value: i64 = rng.random_range(0..=10);
            let item_value: Vec<u8> = vec![i as u8; 10];

            db.insert(
                &[b"count_sum_tree"],
                &key,
                Element::new_item_with_sum_item(item_value, sum_value),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("successful item_with_sum insert");
        }

        // Use max_depth=7, which is less than the max AVL height (9) for 100 elements
        // This should result in truncation
        let max_depth: u8 = 7;
        let tree_depth = calculate_max_tree_depth_from_count(100);
        let expected_chunk_depths = calculate_chunk_depths(tree_depth, max_depth).unwrap();

        let query = PathTrunkChunkQuery::new(vec![b"count_sum_tree".to_vec()], max_depth);

        // Generate the trunk proof
        let proof = db
            .prove_trunk_chunk(&query, grove_version)
            .unwrap()
            .expect("successful trunk proof generation");

        // Verify the trunk proof
        let (root_hash, result) = GroveDb::verify_trunk_chunk_proof(&proof, &query, grove_version)
            .expect("successful trunk proof verification");

        // Verify we got a valid root hash
        assert_ne!(root_hash, [0u8; 32], "root hash should not be all zeros");

        // tree_depth should be 9 (max AVL height for 100 elements)
        assert_eq!(
            result.max_tree_depth, tree_depth,
            "tree depth should match calculated max AVL height"
        );

        // chunk_depths should be [5, 4] for tree_depth=9, max_depth=7
        assert_eq!(
            result.chunk_depths, expected_chunk_depths,
            "chunk depths should split evenly"
        );

        // First chunk has depth 5, so we should get 2^5-1=31 elements
        let first_chunk_depth = expected_chunk_depths[0];
        let expected_elements = (1usize << first_chunk_depth) - 1;
        assert_eq!(
            result.elements.len(),
            expected_elements,
            "should have {} elements in first chunk of depth {}",
            expected_elements,
            first_chunk_depth
        );

        // Should have leaf keys since there's truncation
        assert!(
            !result.leaf_keys.is_empty(),
            "should have leaf keys when tree is truncated"
        );

        // All elements should be ItemWithSumItem
        for (key, element) in &result.elements {
            assert!(
                matches!(element, Element::ItemWithSumItem(..)),
                "element at key {:?} should be ItemWithSumItem, got {:?}",
                key,
                element
            );
        }
    }

    #[test]
    fn test_trunk_proof_with_empty_count_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Insert an empty CountSumTree (no items inside)
        db.insert(
            EMPTY_PATH,
            b"empty_tree",
            Element::empty_count_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful empty_tree insert");

        let query = PathTrunkChunkQuery::new(vec![b"empty_tree".to_vec()], 4);

        // Should succeed, not error
        let proof = db
            .prove_trunk_chunk(&query, grove_version)
            .unwrap()
            .expect("prove should succeed on empty tree");

        // Verify the proof
        let (root_hash, result) = GroveDb::verify_trunk_chunk_proof(&proof, &query, grove_version)
            .expect("verify should succeed on empty tree proof");

        // Root hash should be valid (non-zero -- the root merk has the tree key)
        assert_ne!(root_hash, [0u8; 32], "root hash should not be all zeros");

        // Result should be empty
        assert!(
            result.elements.is_empty(),
            "empty tree should have no elements"
        );
        assert!(
            result.leaf_keys.is_empty(),
            "empty tree should have no leaf keys"
        );
        assert!(
            result.chunk_depths.is_empty(),
            "empty tree should have no chunk depths"
        );
        assert_eq!(result.max_tree_depth, 0, "empty tree should have depth 0");
    }

    /// Verify that trunk proof verification rejects proofs whose target layer
    /// merk_proof has trailing bytes appended.
    #[test]
    fn test_trunk_proof_rejects_trailing_bytes_in_target_layer() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        let mut rng = StdRng::seed_from_u64(99999);

        // Insert a CountSumTree with enough items to generate a trunk proof
        db.insert(
            EMPTY_PATH,
            b"cst",
            Element::empty_count_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert count_sum_tree");

        for i in 0u32..50 {
            let key_num: u8 = rng.random_range(0..=10);
            let mut key = vec![key_num];
            key.extend_from_slice(&i.to_be_bytes());
            db.insert(
                &[b"cst"],
                &key,
                Element::new_sum_item(rng.random_range(0..=5)),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert sum_item");
        }

        let query = PathTrunkChunkQuery::new(vec![b"cst".to_vec()], 3);
        let proof = db
            .prove_trunk_chunk(&query, grove_version)
            .unwrap()
            .expect("prove trunk chunk");

        // Sanity: valid proof verifies
        GroveDb::verify_trunk_chunk_proof(&proof, &query, grove_version)
            .expect("valid proof should verify");

        // Decode the proof, tamper the target layer's merk_proof, re-encode
        let config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        let (decoded_proof, _): (GroveDBProof, _) =
            bincode::decode_from_slice(&proof, config).expect("decode proof");

        let GroveDBProof::V1(mut proof_v1) = decoded_proof else {
            panic!("expected V1 proof from latest version");
        };

        // The target layer is the lower_layers entry for key "cst"
        let target_layer = proof_v1
            .root_layer
            .lower_layers
            .get_mut(b"cst".as_slice())
            .expect("should have cst layer");
        match &mut target_layer.merk_proof {
            ProofBytes::Merk(bytes) => bytes.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]),
            _ => panic!("expected Merk proof bytes"),
        }

        // Re-encode the tampered proof
        let tampered_proof =
            bincode::encode_to_vec(&GroveDBProof::V1(proof_v1), config).expect("re-encode");

        // Verification should fail due to trailing bytes
        let result = GroveDb::verify_trunk_chunk_proof(&tampered_proof, &query, grove_version);
        assert!(
            result.is_err(),
            "trunk proof with trailing bytes in target layer should be rejected"
        );
    }

    /// V1 basic test: verify that the proof is V1 format with ProofBytes::Merk
    /// and that it verifies correctly with a non-empty tree.
    #[test]
    fn test_trunk_proof_v1_basic() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        let mut rng = StdRng::seed_from_u64(42);

        // Insert CountSumTree with 50 items
        db.insert(
            EMPTY_PATH,
            b"cst",
            Element::empty_count_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert cst");

        for i in 0u32..50 {
            let key_num: u8 = rng.random_range(0..=10);
            let mut key = vec![key_num];
            key.extend_from_slice(&i.to_be_bytes());
            db.insert(
                &[b"cst"],
                &key,
                Element::new_sum_item(rng.random_range(0..=5)),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert sum_item");
        }

        let query = PathTrunkChunkQuery::new(vec![b"cst".to_vec()], 3);
        let proof = db
            .prove_trunk_chunk(&query, grove_version)
            .unwrap()
            .expect("prove trunk chunk");

        // Verify the proof succeeds
        let (root_hash, result) = GroveDb::verify_trunk_chunk_proof(&proof, &query, grove_version)
            .expect("V1 trunk proof should verify");

        assert_ne!(root_hash, [0u8; 32]);
        assert!(!result.elements.is_empty());

        // Confirm proof is V1 with ProofBytes::Merk at every layer
        let config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        let decoded: GroveDBProof = bincode::decode_from_slice(&proof, config)
            .expect("decode proof")
            .0;

        let GroveDBProof::V1(proof_v1) = &decoded else {
            panic!("expected V1 proof");
        };

        // Root layer should be ProofBytes::Merk
        assert!(
            matches!(&proof_v1.root_layer.merk_proof, ProofBytes::Merk(_)),
            "root layer should use ProofBytes::Merk"
        );

        // Lower layer (cst) should also be ProofBytes::Merk
        let cst_layer = proof_v1
            .root_layer
            .lower_layers
            .get(b"cst".as_slice())
            .expect("should have cst layer");
        assert!(
            matches!(&cst_layer.merk_proof, ProofBytes::Merk(_)),
            "cst layer should use ProofBytes::Merk"
        );
    }

    /// Security test: V1 trunk proof verification must reject forged count==0
    /// element bytes even when the original value_hash is preserved.
    ///
    /// The attack vector: In KVValueHash proof nodes, the value_hash is stored
    /// separately from the value bytes. An attacker can replace the value bytes
    /// (changing a CountSumTree with count=100 to an empty one with count=0)
    /// while keeping the original value_hash. In V0, the count==0 fast-path
    /// skips the combine_hash verification, so this forgery goes undetected.
    /// V1 must always run the combine_hash check and catch this.
    #[test]
    fn test_trunk_proof_v1_rejects_forged_count_zero() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        let mut rng = StdRng::seed_from_u64(77777);

        // Insert a non-empty CountSumTree with 100 items
        db.insert(
            EMPTY_PATH,
            b"cst",
            Element::empty_count_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert cst");

        for i in 0u32..100 {
            let key_num: u8 = rng.random_range(0..=10);
            let mut key = vec![key_num];
            key.extend_from_slice(&i.to_be_bytes());
            db.insert(
                &[b"cst"],
                &key,
                Element::new_sum_item(rng.random_range(1..=10)),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert sum_item");
        }

        let query = PathTrunkChunkQuery::new(vec![b"cst".to_vec()], 4);

        // Generate a valid V1 proof
        let proof = db
            .prove_trunk_chunk(&query, grove_version)
            .unwrap()
            .expect("prove trunk chunk");

        // Sanity: valid proof verifies
        let (original_root_hash, original_result) =
            GroveDb::verify_trunk_chunk_proof(&proof, &query, grove_version)
                .expect("valid V1 proof should verify");
        assert!(!original_result.elements.is_empty());

        // Decode the V1 proof
        let config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        let decoded: GroveDBProof = bincode::decode_from_slice(&proof, config)
            .expect("decode proof")
            .0;

        let GroveDBProof::V1(mut proof_v1) = decoded else {
            panic!("expected V1 proof");
        };

        // Tamper: find the KVValueHash node for "cst" in the root layer's merk proof
        // and replace the value bytes with an empty CountSumTree element while keeping
        // the original value_hash intact.
        let root_merk_bytes = match &proof_v1.root_layer.merk_proof {
            ProofBytes::Merk(bytes) => bytes.clone(),
            _ => panic!("expected Merk proof bytes"),
        };

        // Decode merk proof ops
        let ops: Vec<Op> = Decoder::new(&root_merk_bytes)
            .collect::<Result<Vec<_>, _>>()
            .expect("decode merk ops");

        // Serialize the forged element (empty CountSumTree with count=0)
        let forged_element = Element::empty_count_sum_tree();
        let forged_value_bytes = forged_element
            .serialize(grove_version)
            .expect("serialize forged element");

        // Find and replace the value bytes for the "cst" key
        let mut tampered_ops: Vec<Op> = Vec::new();
        let mut found_target = false;
        for op in &ops {
            match op {
                Op::Push(Node::KVValueHash(key, _value, value_hash)) if key == b"cst" => {
                    // Replace value bytes with empty-count-tree bytes,
                    // but keep the original value_hash
                    tampered_ops.push(Op::Push(Node::KVValueHash(
                        key.clone(),
                        forged_value_bytes.clone(),
                        *value_hash,
                    )));
                    found_target = true;
                }
                Op::Push(Node::KVValueHashFeatureType(key, _value, value_hash, feature_type))
                    if key == b"cst" =>
                {
                    tampered_ops.push(Op::Push(Node::KVValueHashFeatureType(
                        key.clone(),
                        forged_value_bytes.clone(),
                        *value_hash,
                        feature_type.clone(),
                    )));
                    found_target = true;
                }
                Op::Push(Node::KVValueHashFeatureTypeWithChildHash(
                    key,
                    _value,
                    value_hash,
                    feature_type,
                    child_hash,
                )) if key == b"cst" => {
                    tampered_ops.push(Op::Push(Node::KVValueHashFeatureTypeWithChildHash(
                        key.clone(),
                        forged_value_bytes.clone(),
                        *value_hash,
                        feature_type.clone(),
                        *child_hash,
                    )));
                    found_target = true;
                }
                other => tampered_ops.push(other.clone()),
            }
        }

        assert!(
            found_target,
            "should have found and tampered with the 'cst' node in the merk proof"
        );

        // Re-encode the tampered ops
        let mut tampered_merk_bytes = Vec::new();
        encode_into(tampered_ops.iter(), &mut tampered_merk_bytes);

        // Put tampered bytes back into the proof
        proof_v1.root_layer.merk_proof = ProofBytes::Merk(tampered_merk_bytes);

        // Re-encode the full proof
        let tampered_proof =
            bincode::encode_to_vec(&GroveDBProof::V1(proof_v1), config).expect("re-encode");

        // V1 verification should FAIL because the combine_hash check will detect
        // the mismatch between the forged value bytes and the original value_hash
        let v1_result = GroveDb::verify_trunk_chunk_proof(&tampered_proof, &query, grove_version);
        assert!(
            v1_result.is_err(),
            "V1 should reject forged count==0 element: the combine_hash(value_hash(forged_bytes), \
             NULL_HASH) will not match the original value_hash in the KVValueHash node. \
             Got: {:?}",
            v1_result
        );

        // Now demonstrate the V0 vulnerability: decode the same tampered proof,
        // convert it to a V0 format, and show that V0 verifier incorrectly
        // accepts it because it skips combine_hash when count==0.
        //
        // Re-decode the tampered V1 proof
        let decoded_tampered: GroveDBProof = bincode::decode_from_slice(&tampered_proof, config)
            .expect("decode tampered proof")
            .0;
        let GroveDBProof::V1(tampered_v1) = decoded_tampered else {
            panic!("expected V1 proof");
        };

        // Convert V1 LayerProof to V0 MerkOnlyLayerProof
        fn layer_proof_to_merk_only(
            layer: &crate::operations::proof::LayerProof,
        ) -> crate::operations::proof::MerkOnlyLayerProof {
            let merk_bytes = match &layer.merk_proof {
                ProofBytes::Merk(bytes) => bytes.clone(),
                _ => panic!("expected Merk bytes"),
            };
            let mut lower = std::collections::BTreeMap::new();
            for (k, v) in &layer.lower_layers {
                lower.insert(k.clone(), layer_proof_to_merk_only(v));
            }
            crate::operations::proof::MerkOnlyLayerProof {
                merk_proof: merk_bytes,
                lower_layers: lower,
            }
        }

        let v0_proof = GroveDBProof::V0(crate::operations::proof::GroveDBProofV0 {
            root_layer: layer_proof_to_merk_only(&tampered_v1.root_layer),
            prove_options: tampered_v1.prove_options,
        });

        let v0_proof_bytes = bincode::encode_to_vec(&v0_proof, config).expect("encode V0 proof");

        // V0 verification passes despite the forgery -- this is the vulnerability
        let v0_result = GroveDb::verify_trunk_chunk_proof(&v0_proof_bytes, &query, grove_version);
        assert!(
            v0_result.is_ok(),
            "V0 should (incorrectly) accept the forged count==0 proof due to the fast-path \
             bypass. This demonstrates the vulnerability that V1 fixes. Got error: {:?}",
            v0_result.err()
        );

        // Verify that V0 returns an empty result (the forgery made it think the
        // tree is empty) while the original had 100 elements
        let (v0_root_hash, v0_trunk_result) = v0_result.unwrap();
        assert_eq!(
            v0_root_hash, original_root_hash,
            "V0 root hash should still match (the root merk proof is valid)"
        );
        assert!(
            v0_trunk_result.elements.is_empty(),
            "V0 should return empty elements because count==0 fast-path was taken"
        );
    }

    /// V1 trunk proof with a multi-level path exercises the combine_hash
    /// verification loop across multiple layers.
    #[test]
    fn test_trunk_proof_v1_multi_level_path() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        let mut rng = StdRng::seed_from_u64(55555);

        // Create a 2-level path: root -> subtree -> count_sum_tree
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
            b"cst",
            Element::empty_count_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert cst");

        for i in 0u32..50 {
            let key_num: u8 = rng.random_range(0..=10);
            let mut key = vec![key_num];
            key.extend_from_slice(&i.to_be_bytes());
            db.insert(
                [b"root".as_slice(), b"cst"].as_ref(),
                &key,
                Element::new_sum_item(rng.random_range(1..=10)),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert sum_item");
        }

        let query = PathTrunkChunkQuery::new(vec![b"root".to_vec(), b"cst".to_vec()], 3);
        let proof = db
            .prove_trunk_chunk(&query, grove_version)
            .unwrap()
            .expect("prove trunk chunk");

        let (root_hash, result) = GroveDb::verify_trunk_chunk_proof(&proof, &query, grove_version)
            .expect("V1 multi-level trunk proof should verify");

        assert_ne!(root_hash, [0u8; 32]);
        assert!(!result.elements.is_empty());
        assert!(!result.leaf_keys.is_empty());

        // Confirm it's V1
        let config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        let decoded: GroveDBProof = bincode::decode_from_slice(&proof, config)
            .expect("decode proof")
            .0;
        assert!(matches!(decoded, GroveDBProof::V1(_)));
    }

    /// V1 verification rejects a proof whose target layer has trailing bytes.
    #[test]
    fn test_trunk_proof_v1_rejects_trailing_bytes() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        let mut rng = StdRng::seed_from_u64(88888);

        db.insert(
            EMPTY_PATH,
            b"cst",
            Element::empty_count_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert cst");

        for i in 0u32..30 {
            let key_num: u8 = rng.random_range(0..=10);
            let mut key = vec![key_num];
            key.extend_from_slice(&i.to_be_bytes());
            db.insert(
                &[b"cst"],
                &key,
                Element::new_sum_item(rng.random_range(1..=5)),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert sum_item");
        }

        let query = PathTrunkChunkQuery::new(vec![b"cst".to_vec()], 3);
        let proof = db
            .prove_trunk_chunk(&query, grove_version)
            .unwrap()
            .expect("prove");

        // Sanity: valid proof verifies
        GroveDb::verify_trunk_chunk_proof(&proof, &query, grove_version)
            .expect("valid proof should verify");

        // Decode, tamper target layer with trailing bytes, re-encode
        let config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        let decoded: GroveDBProof = bincode::decode_from_slice(&proof, config)
            .expect("decode proof")
            .0;

        let GroveDBProof::V1(mut proof_v1) = decoded else {
            panic!("expected V1 proof");
        };

        let target_layer = proof_v1
            .root_layer
            .lower_layers
            .get_mut(b"cst".as_slice())
            .expect("should have cst layer");
        match &mut target_layer.merk_proof {
            ProofBytes::Merk(bytes) => bytes.extend_from_slice(&[0xDE, 0xAD]),
            _ => panic!("expected Merk"),
        }

        let tampered =
            bincode::encode_to_vec(&GroveDBProof::V1(proof_v1), config).expect("re-encode");

        let result = GroveDb::verify_trunk_chunk_proof(&tampered, &query, grove_version);
        assert!(
            result.is_err(),
            "V1 trunk proof with trailing bytes should be rejected"
        );
    }

    /// V1 verification rejects a proof targeting a non-count-tree element.
    #[test]
    fn test_trunk_proof_v1_rejects_non_count_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Insert a plain Tree (not CountTree/CountSumTree)
        db.insert(
            EMPTY_PATH,
            b"plain",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert plain tree");

        db.insert(
            [b"plain"].as_ref(),
            b"item",
            Element::new_item(b"data".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item");

        let query = PathTrunkChunkQuery::new(vec![b"plain".to_vec()], 3);
        let result = db.prove_trunk_chunk(&query, grove_version).unwrap();

        // Proving or verifying should fail because plain Tree has no count
        // Either the prover rejects it or the verifier sees no count
        if let Ok(proof) = result {
            let verify_result = GroveDb::verify_trunk_chunk_proof(&proof, &query, grove_version);
            assert!(
                verify_result.is_err(),
                "trunk proof for non-count tree should fail verification"
            );
        }
        // If prove itself failed, that's also acceptable
    }

    /// V1 trunk proof with an empty multi-level path exercises the count==0
    /// path with the combine_hash check across multiple layers.
    #[test]
    fn test_trunk_proof_v1_empty_tree_multi_level() {
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
            b"empty_cst",
            Element::empty_count_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert empty_cst");

        let query = PathTrunkChunkQuery::new(vec![b"root".to_vec(), b"empty_cst".to_vec()], 3);
        let proof = db
            .prove_trunk_chunk(&query, grove_version)
            .unwrap()
            .expect("prove empty tree multi-level");

        let (root_hash, result) = GroveDb::verify_trunk_chunk_proof(&proof, &query, grove_version)
            .expect("V1 empty tree multi-level should verify");

        assert_ne!(root_hash, [0u8; 32]);
        assert!(result.elements.is_empty());
        assert!(result.leaf_keys.is_empty());
        assert!(result.chunk_depths.is_empty());
        assert_eq!(result.max_tree_depth, 0);
    }

    /// V1 trunk proof with different count tree types: CountTree and
    /// ProvableCountSumTree.
    #[test]
    fn test_trunk_proof_v1_count_tree_types() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        let mut rng = StdRng::seed_from_u64(33333);

        // CountTree
        db.insert(
            EMPTY_PATH,
            b"ct",
            Element::empty_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert count_tree");

        for i in 0u32..20 {
            let key_num: u8 = rng.random_range(0..=10);
            let mut key = vec![key_num];
            key.extend_from_slice(&i.to_be_bytes());
            db.insert(
                &[b"ct"],
                &key,
                Element::new_item(vec![i as u8; 8]),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert item into count_tree");
        }

        let query_ct = PathTrunkChunkQuery::new(vec![b"ct".to_vec()], 3);
        let proof_ct = db
            .prove_trunk_chunk(&query_ct, grove_version)
            .unwrap()
            .expect("prove count_tree");

        let (hash_ct, result_ct) =
            GroveDb::verify_trunk_chunk_proof(&proof_ct, &query_ct, grove_version)
                .expect("verify count_tree");
        assert_ne!(hash_ct, [0u8; 32]);
        assert!(!result_ct.elements.is_empty());

        // ProvableCountSumTree
        db.insert(
            EMPTY_PATH,
            b"pcst",
            Element::empty_provable_count_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert provable_count_sum_tree");

        for i in 0u32..20 {
            let key_num: u8 = rng.random_range(0..=10);
            let mut key = vec![key_num];
            key.extend_from_slice(&i.to_be_bytes());
            db.insert(
                &[b"pcst"],
                &key,
                Element::new_sum_item(rng.random_range(1..=10)),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert sum_item into pcst");
        }

        let query_pcst = PathTrunkChunkQuery::new(vec![b"pcst".to_vec()], 3);
        let proof_pcst = db
            .prove_trunk_chunk(&query_pcst, grove_version)
            .unwrap()
            .expect("prove provable_count_sum_tree");

        let (hash_pcst, result_pcst) =
            GroveDb::verify_trunk_chunk_proof(&proof_pcst, &query_pcst, grove_version)
                .expect("verify provable_count_sum_tree");
        assert_ne!(hash_pcst, [0u8; 32]);
        assert!(!result_pcst.elements.is_empty());
    }

    /// V1 forged count on a multi-level path must be rejected: the combine_hash
    /// loop catches the mismatch at the correct layer.
    #[test]
    fn test_trunk_proof_v1_rejects_forged_count_multi_level() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        let mut rng = StdRng::seed_from_u64(66666);

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
            b"cst",
            Element::empty_count_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert cst");

        for i in 0u32..50 {
            let key_num: u8 = rng.random_range(0..=10);
            let mut key = vec![key_num];
            key.extend_from_slice(&i.to_be_bytes());
            db.insert(
                [b"root".as_slice(), b"cst"].as_ref(),
                &key,
                Element::new_sum_item(rng.random_range(1..=10)),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert sum_item");
        }

        let query = PathTrunkChunkQuery::new(vec![b"root".to_vec(), b"cst".to_vec()], 3);
        let proof = db
            .prove_trunk_chunk(&query, grove_version)
            .unwrap()
            .expect("prove");

        // Verify original works
        GroveDb::verify_trunk_chunk_proof(&proof, &query, grove_version).expect("valid proof");

        // Decode and tamper: find "cst" node in second layer, replace value bytes
        let config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        let decoded: GroveDBProof = bincode::decode_from_slice(&proof, config)
            .expect("decode")
            .0;

        let GroveDBProof::V1(mut proof_v1) = decoded else {
            panic!("expected V1");
        };

        // The "cst" key is in the root layer's lower_layers -> "root" lower layer
        let root_lower = proof_v1
            .root_layer
            .lower_layers
            .get_mut(b"root".as_slice())
            .expect("should have root lower layer");

        // The root_lower's merk proof contains the KVValueHash for "cst"
        let merk_bytes = match &root_lower.merk_proof {
            ProofBytes::Merk(bytes) => bytes.clone(),
            _ => panic!("expected Merk"),
        };

        let ops: Vec<Op> = Decoder::new(&merk_bytes)
            .collect::<Result<Vec<_>, _>>()
            .expect("decode ops");

        let forged_element = Element::empty_count_sum_tree();
        let forged_bytes = forged_element
            .serialize(grove_version)
            .expect("serialize forged");

        let mut tampered_ops: Vec<Op> = Vec::new();
        let mut found = false;
        for op in &ops {
            match op {
                Op::Push(Node::KVValueHash(key, _value, vh)) if key == b"cst" => {
                    tampered_ops.push(Op::Push(Node::KVValueHash(
                        key.clone(),
                        forged_bytes.clone(),
                        *vh,
                    )));
                    found = true;
                }
                Op::Push(Node::KVValueHashFeatureType(key, _value, vh, ft)) if key == b"cst" => {
                    tampered_ops.push(Op::Push(Node::KVValueHashFeatureType(
                        key.clone(),
                        forged_bytes.clone(),
                        *vh,
                        ft.clone(),
                    )));
                    found = true;
                }
                Op::Push(Node::KVValueHashFeatureTypeWithChildHash(key, _value, vh, ft, ch))
                    if key == b"cst" =>
                {
                    tampered_ops.push(Op::Push(Node::KVValueHashFeatureTypeWithChildHash(
                        key.clone(),
                        forged_bytes.clone(),
                        *vh,
                        ft.clone(),
                        *ch,
                    )));
                    found = true;
                }
                other => tampered_ops.push(other.clone()),
            }
        }
        assert!(found, "should have found cst node");

        let mut tampered_merk = Vec::new();
        encode_into(tampered_ops.iter(), &mut tampered_merk);
        root_lower.merk_proof = ProofBytes::Merk(tampered_merk);

        let tampered_proof =
            bincode::encode_to_vec(&GroveDBProof::V1(proof_v1), config).expect("encode");

        let result = GroveDb::verify_trunk_chunk_proof(&tampered_proof, &query, grove_version);
        assert!(
            result.is_err(),
            "V1 should reject forged count==0 on multi-level path, got: {:?}",
            result
        );
    }
}
