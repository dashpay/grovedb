//! Simple debug test to check proof generation

#[cfg(test)]
mod tests {
    use grovedb_version::version::GroveVersion;

    use crate::{tests::make_test_grovedb, Element, GroveDb, PathQuery, Query};

    #[test]
    fn debug_simple_proof_check() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

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

        // Insert one item
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

        // Query for all items
        let path_query = PathQuery::new_unsized(vec![b"pcount".to_vec()], Query::new());

        // Prove query
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should prove query");

        eprintln!("Proof length: {} bytes", proof.len());

        // Check if we got KVCount nodes (0x11 is the KVCount node marker)
        let kvcount_count = proof.iter().filter(|&&byte| byte == 0x11).count();
        eprintln!("Number of KVCount nodes in proof: {}", kvcount_count);

        // Let's also check for regular KV nodes (0x10)
        let kv_count = proof.iter().filter(|&&byte| byte == 0x10).count();
        eprintln!("Number of KV nodes in proof: {}", kv_count);

        assert!(
            kvcount_count > 0,
            "Proof should contain at least one KVCount node"
        );
    }
}
