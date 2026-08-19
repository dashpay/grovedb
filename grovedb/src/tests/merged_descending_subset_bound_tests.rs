//! A merged proof must stay subset-verifiable in both directions.
//!
//! Shape from production (dashpay/platform's document cursor proofs): one
//! layer holds a documents subtree (`"0"`), a queried index subtree
//! (`"firstName"`), and unqueried sibling index subtrees. The cursor
//! branch selects a key inside `"0"`, the main branch descends through
//! `"firstName"`, and the two are merged — direction-aligned, as the
//! merge requires — into one proof. Verifying the cursor branch alone
//! (`verify_subset_query`) must succeed regardless of direction: the
//! ascending and descending merged proofs commit the same data, and a
//! subset query names keys only, never order-dependent content.

#[cfg(test)]
mod tests {
    use grovedb_version::version::GroveVersion;

    use crate::{
        tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
        Element, GroveDb, PathQuery, Query, SizedQuery,
    };

    /// `[TEST_LEAF, person]` with a documents subtree `"0"` (two items),
    /// a queried index subtree `"firstName"` (one item), and an
    /// unqueried sibling index subtree `"middleName"` that the proof
    /// will abridge.
    fn build_layered_fixture(gv: &GroveVersion) -> TempGroveDb {
        let db = make_test_grovedb(gv);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"person",
            Element::empty_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("insert person tree");
        for subtree in [b"0".as_ref(), b"firstName", b"middleName"] {
            db.insert(
                [TEST_LEAF, b"person"].as_ref(),
                subtree,
                Element::empty_tree(),
                None,
                None,
                gv,
            )
            .unwrap()
            .expect("insert layer subtree");
        }
        for doc in [b"docA".as_ref(), b"docB"] {
            db.insert(
                [TEST_LEAF, b"person", b"0"].as_ref(),
                doc,
                Element::new_item(doc.to_vec()),
                None,
                None,
                gv,
            )
            .unwrap()
            .expect("insert document");
        }
        db.insert(
            [TEST_LEAF, b"person", b"firstName"].as_ref(),
            b"Chris",
            Element::new_item(b"Chris".to_vec()),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("insert index row");
        db.insert(
            [TEST_LEAF, b"person", b"middleName"].as_ref(),
            b"x",
            Element::new_item(b"x".to_vec()),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("insert unqueried index row");
        db
    }

    fn cursor_and_main_queries(left_to_right: bool) -> (PathQuery, PathQuery) {
        let mut cursor_q = Query::new_with_direction(left_to_right);
        cursor_q.insert_key(b"docA".to_vec());
        let cursor_pq = PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"person".to_vec(), b"0".to_vec()],
            SizedQuery::new(cursor_q, None, None),
        );

        let mut main_q = Query::new_with_direction(left_to_right);
        main_q.insert_range_from(b"Chris".to_vec()..);
        let main_pq = PathQuery::new(
            vec![
                TEST_LEAF.to_vec(),
                b"person".to_vec(),
                b"firstName".to_vec(),
            ],
            SizedQuery::new(main_q, None, None),
        );

        (cursor_pq, main_pq)
    }

    fn merged_subset_round_trip(left_to_right: bool) {
        let gv = GroveVersion::latest();
        let db = build_layered_fixture(gv);
        let (cursor_pq, main_pq) = cursor_and_main_queries(left_to_right);

        let merged = PathQuery::merge(vec![&cursor_pq, &main_pq], gv).expect("aligned merge");
        let proof = db
            .prove_query(&merged, None, gv)
            .unwrap()
            .expect("prove merged query");

        // The whole merged query must verify...
        GroveDb::verify_query(&proof, &merged, gv)
            .unwrap_or_else(|e| panic!("full merged verify (ltr={left_to_right}): {e}"));

        // ...and so must the cursor branch alone: subset verification is
        // how a client extracts one branch (e.g. the pagination cursor
        // document) from a combined proof.
        let (_, proved) = GroveDb::verify_subset_query(&proof, &cursor_pq, gv)
            .unwrap_or_else(|e| panic!("subset cursor verify (ltr={left_to_right}): {e}"));
        assert_eq!(proved.len(), 1, "exactly the cursor document");
        assert_eq!(proved[0].1, b"docA".to_vec());
    }

    #[test]
    fn merged_ascending_proof_subset_verifies_cursor_branch() {
        merged_subset_round_trip(true);
    }

    /// FAILS ON DEVELOP — deliberately not ignored: this test IS the bug
    /// report.
    ///
    /// The full merged verify passes, but `verify_subset_query` of the
    /// cursor branch rejects with "Cannot verify lower bound of queried
    /// range". The prover classified the shared layer against the merged
    /// (descending) query and emitted its ops inverted; subset
    /// verification re-derives that layer from the cursor path query
    /// alone, and `query_items_at_path` synthesizes path-component levels
    /// with a hardcoded ascending direction
    /// (`SinglePathSubquery::from_key_when_in_path`), so the verifier
    /// runs the ascending bound-witness check against inverted pushes and
    /// trips on the first hash-abridged sibling.
    ///
    /// Note for the fix: no fixed direction is correct for synthesized
    /// levels — the generating query is unknowable from the subset query.
    /// Hardcoding ascending is this bug; inheriting the subset query's
    /// direction instead breaks proofs whose generation synthesized the
    /// same level ascending (verified against dashpay/platform's
    /// protocol-v13 frozen ascending cursor-proof test).
    #[test]
    fn merged_descending_proof_subset_verifies_cursor_branch() {
        merged_subset_round_trip(false);
    }
}
