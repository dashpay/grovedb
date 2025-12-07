//! Debug test to check if proof works immediately after insertion

#[cfg(test)]
mod tests {
    use grovedb_version::version::GroveVersion;

    use crate::{tests::make_test_grovedb, Element, GroveDb, PathQuery, Query};

    #[test]
    fn debug_immediate_proof_check() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        eprintln!("\n=== Creating ProvableCountTree ===");
        // Insert a ProvableCountTree
        db.insert(
            &[] as &[&[u8]],
            b"pcount",
            Element::empty_provable_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert provable count tree");

        eprintln!("\n=== Inserting items into ProvableCountTree ===");
        // Insert items
        db.insert(
            &[b"pcount"],
            b"alice",
            Element::new_item(b"alice_value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert alice");

        db.insert(
            &[b"pcount"],
            b"bob",
            Element::new_item(b"bob_value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert bob");

        db.insert(
            &[b"pcount"],
            b"charlie",
            Element::new_item(b"charlie_value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert charlie");

        eprintln!("\n=== Creating query for all items ===");
        // Query for all items
        let path_query = PathQuery::new_unsized(vec![b"pcount".to_vec()], Query::new());

        eprintln!("\n=== Proving query ===");
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove query");

        eprintln!("Proof length: {} bytes", proof.len());

        eprintln!("\n=== Verifying proof ===");

        eprintln!("\n=== Verifying raw query ===");
        let (verified_hash, results) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version)
                .expect("should verify query");

        eprintln!("Verified hash: {}", hex::encode(verified_hash));
        eprintln!("Results: {:?}", results);

        // Check if we got KVCount nodes
        // KVCount node marker is 0x11
        let kvcount_count = proof.iter().filter(|&&byte| byte == 0x11).count();
        eprintln!("Number of KVCount nodes in proof: {}", kvcount_count);

        assert!(
            kvcount_count > 0,
            "Proof should contain at least one KVCount node"
        );

        // Get the expected root hash
        let root_hash = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get root hash");
        eprintln!("Expected root hash: {}", hex::encode(root_hash));

        assert_eq!(
            verified_hash, root_hash,
            "Verified hash should match root hash"
        );
    }
}
