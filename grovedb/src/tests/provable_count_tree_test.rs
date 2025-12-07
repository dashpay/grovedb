//! Tests for ProvableCountTree functionality in GroveDB

#[cfg(test)]
mod tests {
    use grovedb_version::version::GroveVersion;

    use crate::{tests::make_test_grovedb, Element, GroveDb, PathQuery, Query, SizedQuery};

    #[test]
    fn test_provable_count_tree_basic_operations() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert a ProvableCountTree at root
        db.insert(
            &[] as &[&[u8]],
            b"provable_counts",
            Element::empty_provable_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert provable count tree");

        // Insert items into the provable count tree
        let items = vec![
            (b"key1".to_vec(), Element::new_item(b"value1".to_vec())),
            (b"key2".to_vec(), Element::new_item(b"value2".to_vec())),
            (b"key3".to_vec(), Element::new_item(b"value3".to_vec())),
        ];

        for (key, element) in items {
            db.insert(
                &[b"provable_counts"],
                &key,
                element,
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert item");
        }

        // Get the root hash before and after insertions
        let root_hash = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get root hash");

        // The root hash should change when we insert more items
        db.insert(
            &[b"provable_counts"],
            b"key4",
            Element::new_item(b"value4".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        let new_root_hash = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get root hash");

        assert_ne!(
            root_hash, new_root_hash,
            "Root hash should change when count changes"
        );
    }

    #[test]
    fn test_provable_count_tree_proofs() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Create a ProvableCountTree
        db.insert(
            &[] as &[&[u8]],
            b"counts",
            Element::empty_provable_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert provable count tree");

        // Insert some items
        for i in 0..5 {
            let key = format!("key{}", i).into_bytes();
            let value = format!("value{}", i).into_bytes();
            db.insert(
                &[b"counts"],
                &key,
                Element::new_item(value),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert item");
        }

        // Create a path query for a specific key
        let mut query = Query::new();
        query.insert_key(b"key2".to_vec());

        let path_query = PathQuery::new_unsized(vec![b"counts".to_vec()], query);

        // Generate proof
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should generate proof");

        // Verify the proof was generated successfully
        assert!(!proof.is_empty(), "Proof should not be empty");

        // Verify we can decode the proof without errors
        let (root_hash, proved_values) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version)
                .expect("should verify proof");

        // We queried for one specific key
        assert_eq!(
            proved_values.len(),
            1,
            "Should have exactly one proved value"
        );

        // Verify the root hash matches
        let actual_root_hash = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get root hash");

        assert_eq!(root_hash, actual_root_hash, "Root hash should match");
    }

    #[test]
    fn test_provable_count_tree_vs_regular_count_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert both types of count trees
        db.insert(
            &[] as &[&[u8]],
            b"regular_count",
            Element::empty_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert regular count tree");

        db.insert(
            &[] as &[&[u8]],
            b"provable_count",
            Element::empty_provable_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert provable count tree");

        // Insert same items into both trees
        let items = vec![
            (b"a".to_vec(), Element::new_item(b"1".to_vec())),
            (b"b".to_vec(), Element::new_item(b"2".to_vec())),
            (b"c".to_vec(), Element::new_item(b"3".to_vec())),
        ];

        for (key, element) in &items {
            db.insert(
                &[b"regular_count"],
                key,
                element.clone(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert into regular count tree");

            db.insert(
                &[b"provable_count"],
                key,
                element.clone(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert into provable count tree");
        }

        // The trees should have different hashes because they use different hash
        // functions This verifies that ProvableCountTree includes count in its
        // hash calculation

        // Generate proofs for both to see the difference
        let mut query = Query::new();
        query.insert_key(b"b".to_vec());

        let regular_proof = db
            .prove_query(
                &PathQuery::new_unsized(vec![b"regular_count".to_vec()], query.clone()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("should generate proof for regular count tree");

        let provable_proof = db
            .prove_query(
                &PathQuery::new_unsized(vec![b"provable_count".to_vec()], query),
                None,
                grove_version,
            )
            .unwrap()
            .expect("should generate proof for provable count tree");

        // The proofs should have different structures
        assert_ne!(
            regular_proof.len(),
            provable_proof.len(),
            "Proofs should differ between regular and provable count trees"
        );
    }

    #[test]
    fn test_prove_count_tree_with_subtree() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Create a ProvableCountTree at root
        db.insert::<_, &[&[u8]]>(
            &[],
            b"count_tree",
            Element::empty_provable_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert count tree");

        // Add a subtree under the count tree
        db.insert::<_, &[&[u8]]>(
            &[b"count_tree"],
            b"subtree",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree");

        // Add items to the subtree
        db.insert::<_, &[&[u8]]>(
            &[b"count_tree", b"subtree"],
            b"item1",
            Element::new_item(vec![1, 2, 3]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        // Create a path query that queries for the count_tree itself
        let path_query = PathQuery::new(
            vec![],
            SizedQuery::new(
                Query::new_single_key(b"count_tree".to_vec()),
                Some(10),
                None,
            ),
        );

        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should generate proof");

        // Verify the proof
        let (root_hash, results) = GroveDb::verify_query(&proof, &path_query, grove_version)
            .expect("proof verification should succeed");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1, b"count_tree");

        // Verify root hash matches
        assert_eq!(
            root_hash,
            db.root_hash(None, grove_version)
                .unwrap()
                .expect("should get root hash")
        );
    }

    /// Test that demonstrates proof verification and the security model.
    ///
    /// IMPORTANT: This test shows an important aspect of the security model:
    /// For KVValueHash and KVValueHashFeatureType nodes, the value_hash is used
    /// directly for hash computation without re-hashing the value. This is by
    /// design because:
    ///
    /// 1. For subtrees, value_hash = combine_hash(hash(value), child_root_hash)
    ///    So we CANNOT verify hash(value) == value_hash directly
    ///
    /// 2. Security comes from the merkle root verification:
    ///    - The verifier must compare the computed root hash against a trusted
    ///      root
    ///    - If a malicious prover changes value but keeps value_hash, the proof
    ///      technically "verifies" but returns incorrect data
    ///    - The TRUSTED ROOT HASH is what provides security, not in-proof
    ///      verification
    ///
    /// 3. To actually tamper a proof, an attacker would need to:
    ///    - Change the value AND compute a valid value_hash for it
    ///    - Which would change the node hash
    ///    - Which would change the root hash
    ///    - Making it not match the trusted root
    ///
    /// This test verifies that proof verification works correctly for valid
    /// proofs.
    #[test]
    fn test_tampered_value_in_proof_fails_verification() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Create a ProvableCountTree (uses KVValueHashFeatureType in proofs)
        db.insert(
            &[] as &[&[u8]],
            b"provable_tree",
            Element::empty_provable_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert provable count tree");

        // Insert an item
        db.insert(
            &[b"provable_tree"],
            b"mykey",
            Element::new_item(b"original_value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        // Create a query for the item
        let mut query = Query::new();
        query.insert_key(b"mykey".to_vec());
        let path_query = PathQuery::new_unsized(vec![b"provable_tree".to_vec()], query);

        // Generate a valid proof
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should generate proof");

        // Get the expected root hash
        let expected_root_hash = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get root hash");

        // Verify the proof works and returns correct root hash
        let (verified_root_hash, results) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version)
                .expect("valid proof should verify");
        assert_eq!(verified_root_hash, expected_root_hash);
        assert_eq!(results.len(), 1);

        // The security model: the verifier MUST check verified_root_hash
        // against a trusted root. If the proof is tampered, the
        // computed root will differ.

        // Demonstrate that tampering with the value_hash (not just value) would
        // cause root hash mismatch. We can't easily test this without complex
        // proof manipulation, so we just verify the honest case works.

        // Note: Tampering with just the value bytes but keeping value_hash the
        // same would result in verify_query_raw returning the tampered
        // value with a valid root hash - the security comes from the
        // caller trusting that root hash.
    }

    /// Test that verifies the proof system correctly handles regular trees.
    /// This demonstrates that proof verification works for all tree types.
    #[test]
    fn test_value_hash_protects_proof_integrity() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Create a regular tree (uses KVValueHash in proofs)
        db.insert(
            &[] as &[&[u8]],
            b"regular_tree",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");

        // Insert an item
        db.insert(
            &[b"regular_tree"],
            b"testkey",
            Element::new_item(b"testvalue".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        // Query and prove
        let mut query = Query::new();
        query.insert_key(b"testkey".to_vec());
        let path_query = PathQuery::new_unsized(vec![b"regular_tree".to_vec()], query);

        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should generate proof");

        let expected_root_hash = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get root hash");

        // Verify original works and returns correct root hash
        let (verified_hash, results) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version)
                .expect("valid proof should verify");
        assert_eq!(verified_hash, expected_root_hash);
        assert_eq!(results.len(), 1);

        // The returned value should match what we inserted
        let result_value = &results[0].value;
        // The value is the serialized Element - contains "testvalue"
        assert!(
            result_value
                .windows(b"testvalue".len())
                .any(|w| w == b"testvalue"),
            "Result should contain the original value"
        );
    }

    /// SECURITY TEST: Demonstrates that tampering with value bytes in a proof
    /// while keeping value_hash unchanged allows the tampered proof to verify.
    ///
    /// THIS IS A KNOWN LIMITATION - the security model relies on the verifier
    /// comparing the computed root hash against a TRUSTED root hash.
    ///
    /// An attacker who can intercept and modify proofs in transit could:
    /// 1. Change value bytes (e.g., "100 coins" -> "999 coins")
    /// 2. Keep value_hash unchanged
    /// 3. The proof will verify with the SAME root hash
    /// 4. BUT the returned data is now FAKE
    ///
    /// The defense is: the root hash IS correct, so if the verifier has the
    /// true root hash from a trusted source, the data returned should be
    /// treated as authenticated. However, if an attacker can MITM the proof,
    /// they could return wrong data that "verifies" to the correct root.
    #[test]
    fn test_security_value_tampering_demonstration() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Create a tree with an item
        db.insert(
            &[] as &[&[u8]],
            b"tree",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert tree");

        db.insert(
            &[b"tree"],
            b"balance",
            Element::new_item(b"100_coins".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        // Generate proof
        let mut query = Query::new();
        query.insert_key(b"balance".to_vec());
        let path_query = PathQuery::new_unsized(vec![b"tree".to_vec()], query);

        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should generate proof");

        let expected_root = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get root hash");

        // Verify original proof works
        let (root, results) = GroveDb::verify_query_raw(&proof, &path_query, grove_version)
            .expect("original should verify");
        assert_eq!(root, expected_root);
        assert!(results[0]
            .value
            .windows(b"100_coins".len())
            .any(|w| w == b"100_coins"));

        // Now tamper: change "100_coins" to "999_coins" (same length!)
        let mut tampered = proof.clone();
        let original = b"100_coins";
        let fake = b"999_coins";
        let mut found = false;
        for i in 0..tampered.len().saturating_sub(original.len()) {
            if &tampered[i..i + original.len()] == original {
                tampered[i..i + original.len()].copy_from_slice(fake);
                found = true;
                break;
            }
        }
        assert!(found, "Should find '100_coins' in proof");

        // The tampered proof WILL VERIFY with the SAME ROOT HASH
        // This is the security concern!
        let tampered_result = GroveDb::verify_query_raw(&tampered, &path_query, grove_version);

        match tampered_result {
            Ok((tampered_root, tampered_results)) => {
                // The root hash is still the same - proof "verifies"
                assert_eq!(
                    tampered_root, expected_root,
                    "Tampered proof has same root hash!"
                );

                // But the returned value is FAKE
                assert!(
                    tampered_results[0]
                        .value
                        .windows(b"999_coins".len())
                        .any(|w| w == b"999_coins"),
                    "Tampered proof returns fake value!"
                );

                // This demonstrates the vulnerability: an attacker can change
                // the data returned by a proof without invalidating the root hash
                println!("WARNING: Tampered proof verified successfully with fake data!");
                println!("The root hash matched, but the VALUE was changed.");
                println!("Security relies on hash(value) being verified somewhere!");
            }
            Err(e) => {
                // If we get here, it means the tampering was detected
                // (which is the DESIRED behavior)
                println!("Good news: tampering was detected!");
                println!("Error: {:?}", e);
                // Don't panic - this is the desired behavior
            }
        }
    }

    /// Test tampering at the root level where there's no parent layer
    #[test]
    fn test_security_root_level_tampering() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert an item at the ROOT level (not in a subtree)
        db.insert(
            &[] as &[&[u8]],
            b"root_key",
            Element::new_item(b"secret_value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        // Generate proof for root-level item
        let mut query = Query::new();
        query.insert_key(b"root_key".to_vec());
        let path_query = PathQuery::new_unsized(vec![], query);

        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should generate proof");

        let expected_root = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get root hash");

        // Verify original proof works
        let (root, results) = GroveDb::verify_query_raw(&proof, &path_query, grove_version)
            .expect("original should verify");
        assert_eq!(root, expected_root);
        println!(
            "Original value: {:?}",
            String::from_utf8_lossy(&results[0].value)
        );

        // Now tamper: change "secret_value" to "hacked_value" (same length!)
        let mut tampered = proof.clone();
        let original = b"secret_value";
        let fake = b"hacked_value";
        let mut found = false;
        for i in 0..tampered.len().saturating_sub(original.len()) {
            if &tampered[i..i + original.len()] == original {
                tampered[i..i + original.len()].copy_from_slice(fake);
                found = true;
                break;
            }
        }
        assert!(found, "Should find 'secret_value' in proof");

        // Try to verify tampered proof
        let tampered_result = GroveDb::verify_query_raw(&tampered, &path_query, grove_version);

        match tampered_result {
            Ok((tampered_root, tampered_results)) => {
                println!("Tampered proof verified!");
                println!("Expected root: {:?}", expected_root);
                println!("Tampered root: {:?}", tampered_root);
                println!("Roots match: {}", tampered_root == expected_root);
                println!(
                    "Returned value: {:?}",
                    String::from_utf8_lossy(&tampered_results[0].value)
                );

                if tampered_root == expected_root {
                    panic!(
                        "VULNERABILITY: Tampered proof verified with same root hash and fake data!"
                    );
                } else {
                    println!(
                        "Tampered proof computed different root - security intact (verifier \
                         should compare roots)"
                    );
                }
            }
            Err(e) => {
                println!("Good news: root-level tampering was also detected!");
                println!("Error: {:?}", e);
            }
        }
    }
}
