//! Tests for proof depth limit enforcement (H3 fix).
//!
//! Verifies that proof generation and verification reject queries and proofs
//! that exceed `MAX_PROOF_DEPTH`, preventing stack overflow from deeply nested
//! subqueries or maliciously crafted proofs.

#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::{
        query::{QueryItem, SubqueryBranch},
        Query,
    };
    use grovedb_version::version::GroveVersion;

    use crate::{
        operations::proof::MAX_PROOF_DEPTH,
        tests::{common::EMPTY_PATH, make_empty_grovedb},
        Element, GroveDb, PathQuery, SizedQuery,
    };

    /// Build a GroveDB with a deeply nested chain of trees, each level
    /// containing a single subtree named by its depth index.
    ///
    /// Structure:
    ///   root -> "deep" -> "0" -> "1" -> ... -> "depth-1" (leaf item)
    fn make_deep_chain(depth: usize) -> crate::tests::TempGroveDb {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Insert the top-level tree "deep" under root
        db.insert(
            EMPTY_PATH,
            b"deep",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert root tree");

        // Build the chain: each level i has a subtree key = i.to_string()
        let mut path_vecs: Vec<Vec<u8>> = vec![b"deep".to_vec()];
        for i in 0..depth {
            let key = i.to_string().into_bytes();
            let path_slices: Vec<&[u8]> = path_vecs.iter().map(|p| p.as_slice()).collect();
            db.insert(
                path_slices.as_slice(),
                &key,
                if i == depth - 1 {
                    // Leaf level: insert an item so the query terminates
                    Element::new_item(b"leaf_value".to_vec())
                } else {
                    Element::empty_tree()
                },
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert at depth");

            if i < depth - 1 {
                path_vecs.push(key);
            }
        }
        db
    }

    /// Build a `PathQuery` that recurses through a chain of default subqueries
    /// to `depth` levels.
    ///
    /// Each level selects all keys and has a default subquery that does the same,
    /// creating `depth` levels of recursion.
    fn make_recursive_path_query(depth: usize) -> PathQuery {
        // Build the query from the inside out: the innermost query selects all
        // keys with no further subqueries.
        let mut query = Query::new();
        query.insert_all();

        for _ in 0..depth {
            let mut outer = Query::new();
            outer.insert_all();
            outer.set_subquery(query);
            query = outer;
        }

        PathQuery::new(
            vec![b"deep".to_vec()],
            SizedQuery::new(query, Some(100), None),
        )
    }

    // =========================================================================
    // Proof generation depth limit tests
    // =========================================================================

    #[test]
    fn proof_generation_succeeds_at_reasonable_depth() {
        // A depth of 20 is well within the limit and should work fine.
        let grove_version = GroveVersion::latest();
        let depth = 20;
        let db = make_deep_chain(depth);
        let path_query = make_recursive_path_query(depth);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("proof generation should succeed at depth 20");

        // Verify the proof also succeeds
        let (_, results) = GroveDb::verify_query(&proof_bytes, &path_query, grove_version)
            .expect("proof verification should succeed at depth 20");

        // We should get at least the leaf item
        assert!(
            !results.is_empty(),
            "query at depth 20 should return results"
        );
    }

    #[test]
    fn proof_generation_rejects_depth_exceeding_limit() {
        // Build a chain deeper than MAX_PROOF_DEPTH and a query that traverses
        // all levels. Proof generation must return an error rather than
        // overflowing the stack.
        //
        // We run this in a thread with a large stack because building and
        // traversing 130+ levels of GroveDB trees needs substantial stack
        // space even with the depth limit check in place (the tree construction
        // and the first 128 recursion levels before the check triggers).
        let result = std::thread::Builder::new()
            .name("deep_proof_test".to_string())
            .stack_size(64 * 1024 * 1024) // 64 MB stack
            .spawn(|| {
                let grove_version = GroveVersion::latest();
                let depth = MAX_PROOF_DEPTH + 2;
                let db = make_deep_chain(depth);
                let path_query = make_recursive_path_query(depth);

                let result = db.prove_query(&path_query, None, grove_version);
                let err = result
                    .unwrap()
                    .expect_err(
                        "proof generation should fail when depth exceeds MAX_PROOF_DEPTH",
                    );

                let err_string = format!("{}", err);
                assert!(
                    err_string.contains("maximum depth limit"),
                    "error should mention depth limit, got: {}",
                    err_string
                );
            })
            .expect("failed to spawn test thread")
            .join();

        // If the thread panicked, propagate the panic
        result.expect("test thread panicked");
    }

    // =========================================================================
    // Proof verification depth limit tests
    // =========================================================================

    #[test]
    fn max_proof_depth_constant_is_reasonable() {
        // Verify the depth limit constant is within a practical range.
        assert!(
            MAX_PROOF_DEPTH >= 64,
            "MAX_PROOF_DEPTH should be at least 64 for practical use"
        );
        assert!(
            MAX_PROOF_DEPTH <= 256,
            "MAX_PROOF_DEPTH should not be unreasonably large"
        );
    }

    #[test]
    fn proof_round_trip_at_moderate_depth() {
        // Verify that proof generation and verification round-trip correctly
        // at a moderate depth (10 levels).
        let grove_version = GroveVersion::latest();
        let depth = 10;
        let db = make_deep_chain(depth);
        let path_query = make_recursive_path_query(depth);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("proof generation should succeed");

        let (root_hash, results) = GroveDb::verify_query(&proof_bytes, &path_query, grove_version)
            .expect("proof verification should succeed");

        assert_ne!(
            root_hash, [0u8; 32],
            "root hash should be non-zero after verification"
        );
        assert!(
            !results.is_empty(),
            "results should contain at least the leaf item"
        );
    }
}
