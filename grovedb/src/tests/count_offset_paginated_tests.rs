//! End-to-end tests for offset-paginated proofs against
//! `ProvableCountTree` / `ProvableCountSumTree` merks.
//!
//! Lives at the GroveDB layer (not the merk layer) so the path-query
//! navigation + chain check is exercised — the merk-level unit tests
//! in `merk/src/proofs/query/count_offset/tests.rs` already cover the
//! pure prover/verifier roundtrip on a single merk.

#[cfg(test)]
mod tests {
    use grovedb_version::version::GroveVersion;

    use crate::{
        operations::proof::util::ProvedPathKeyValues, tests::make_test_grovedb, Element, GroveDb,
        PathQuery, Query, SizedQuery,
    };

    /// Build a fresh DB with `count_tree` (an empty `ProvableCountTree`)
    /// at the root, then insert keys "a" .. ('a' + n) into it, each
    /// mapped to a value of `format!("v_{}", key)`. Returns the DB and
    /// the keys as a `Vec<Vec<u8>>` in ascending order.
    fn make_provable_count_tree_with_n_items(
        n: u8,
        grove_version: &GroveVersion,
    ) -> (crate::tests::TempGroveDb, Vec<Vec<u8>>) {
        assert!(n <= 26, "fixture supports up to 26 single-letter keys");
        let db = make_test_grovedb(grove_version);
        db.insert(
            &[] as &[&[u8]],
            b"counts",
            Element::empty_provable_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert tree");
        let mut keys = Vec::with_capacity(n as usize);
        for i in 0..n {
            let key = vec![b'a' + i];
            let value = format!("v_{}", String::from_utf8_lossy(&key)).into_bytes();
            db.insert(
                &[b"counts"],
                key.as_slice(),
                Element::new_item(value),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert item");
            keys.push(key);
        }
        (db, keys)
    }

    /// Round-trip a single-range offset+limit query against a
    /// `ProvableCountTree`. Returns the verified items so callers can
    /// assert on key/value contents.
    fn round_trip_offset(
        db: &crate::tests::TempGroveDb,
        path: Vec<Vec<u8>>,
        query: Query,
        limit: Option<u16>,
        offset: Option<u16>,
        grove_version: &GroveVersion,
    ) -> ProvedPathKeyValues {
        let sized = SizedQuery::new(query, limit, offset);
        let path_query = PathQuery::new(path, sized);

        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove offset-paginated query");
        assert!(!proof.is_empty(), "proof bytes should be non-empty");

        let (root_hash, proved) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).expect("verify");
        let actual_root = db.root_hash(None, grove_version).unwrap().expect("root");
        assert_eq!(
            root_hash, actual_root,
            "verifier root hash should match the DB's actual root hash"
        );
        proved
    }

    fn proved_keys(proved: &ProvedPathKeyValues) -> Vec<Vec<u8>> {
        proved.iter().map(|p| p.key.clone()).collect()
    }

    #[test]
    fn end_to_end_offset_5_limit_3_ascending() {
        let v = GroveVersion::latest();
        let (db, _) = make_provable_count_tree_with_n_items(15, v);
        let mut q = Query::new();
        q.insert_range_inclusive(b"a".to_vec()..=b"o".to_vec());

        let proved = round_trip_offset(&db, vec![b"counts".to_vec()], q, Some(3), Some(5), v);
        assert_eq!(
            proved_keys(&proved),
            vec![b"f".to_vec(), b"g".to_vec(), b"h".to_vec()],
            "ascending: offset 5 + limit 3 should return f,g,h"
        );
    }

    #[test]
    fn end_to_end_offset_5_limit_3_descending() {
        let v = GroveVersion::latest();
        let (db, _) = make_provable_count_tree_with_n_items(15, v);
        let mut q = Query::new_with_direction(false); // right-to-left
        q.insert_range_inclusive(b"a".to_vec()..=b"o".to_vec());

        let proved = round_trip_offset(&db, vec![b"counts".to_vec()], q, Some(3), Some(5), v);
        assert_eq!(
            proved_keys(&proved),
            vec![b"j".to_vec(), b"i".to_vec(), b"h".to_vec()],
            "descending: offset 5 + limit 3 should return j,i,h"
        );
    }

    #[test]
    fn end_to_end_offset_past_end_returns_empty() {
        let v = GroveVersion::latest();
        let (db, _) = make_provable_count_tree_with_n_items(15, v);
        let mut q = Query::new();
        q.insert_range_inclusive(b"a".to_vec()..=b"o".to_vec());

        let proved = round_trip_offset(
            &db,
            vec![b"counts".to_vec()],
            q,
            Some(3),
            Some(100), // larger than the 15-item population
            v,
        );
        assert!(
            proved.is_empty(),
            "offset past the end yields zero returned items"
        );
    }

    #[test]
    fn end_to_end_offset_in_middle_of_partial_range() {
        let v = GroveVersion::latest();
        let (db, _) = make_provable_count_tree_with_n_items(15, v);
        // Restrict the range so some items are out-of-range, exercising
        // the Disjoint-subtree collapse alongside the offset machinery.
        let mut q = Query::new();
        q.insert_range_inclusive(b"c".to_vec()..=b"l".to_vec());

        let proved = round_trip_offset(&db, vec![b"counts".to_vec()], q, Some(3), Some(4), v);
        assert_eq!(
            proved_keys(&proved),
            vec![b"g".to_vec(), b"h".to_vec(), b"i".to_vec()],
            "ascending c..=l, offset 4 + limit 3 should return g,h,i"
        );
    }

    #[test]
    fn end_to_end_offset_with_limit_none_returns_remainder() {
        let v = GroveVersion::latest();
        let (db, _) = make_provable_count_tree_with_n_items(15, v);
        let mut q = Query::new();
        q.insert_range_inclusive(b"c".to_vec()..=b"l".to_vec());

        let proved = round_trip_offset(
            &db,
            vec![b"counts".to_vec()],
            q,
            None, // no limit → all remaining in-range
            Some(3),
            v,
        );
        assert_eq!(
            proved_keys(&proved),
            vec![
                b"f".to_vec(),
                b"g".to_vec(),
                b"h".to_vec(),
                b"i".to_vec(),
                b"j".to_vec(),
                b"k".to_vec(),
                b"l".to_vec(),
            ],
            "c..=l offset 3 with no limit returns f..l (7 items)"
        );
    }

    #[test]
    fn end_to_end_offset_rejects_with_subquery() {
        // Sanity: an offset query that fails the syntactic
        // `validate_count_offset_paginated` check must be rejected at
        // the prover entry, not silently fall through to the regular
        // proof path.
        let v = GroveVersion::latest();
        let (db, _) = make_provable_count_tree_with_n_items(5, v);

        let mut q = Query::new();
        q.insert_range_inclusive(b"a".to_vec()..=b"e".to_vec());
        // Add a default subquery branch — out-of-scope shape.
        q.default_subquery_branch.subquery = Some(Box::new(Query::new()));

        let path_query = PathQuery::new(
            vec![b"counts".to_vec()],
            SizedQuery::new(q, Some(3), Some(1)),
        );
        let result = db.prove_query(&path_query, None, v).unwrap();
        assert!(
            result.is_err(),
            "prover must reject offset on a query with a default subquery branch"
        );
    }

    #[test]
    fn end_to_end_offset_on_provable_count_sum_tree() {
        // `ProvableCountSumTree` shares the same `node_hash_with_count`
        // hashing rule as `ProvableCountTree` (the sum is stored on the
        // node but not bound to the hash), so the same `HashWithCount`
        // collapse op works for it. This test exercises that path
        // end-to-end through the grovedb layer.
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            &[] as &[&[u8]],
            b"counts_sum",
            Element::empty_provable_count_sum_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert provable count-sum tree");
        for i in 0..15u8 {
            let key = vec![b'a' + i];
            // `Element::new_item` stores plain Items, which contribute
            // 1 to count and 0 to sum (sum gates only fire for
            // sum-flavored values).
            db.insert(
                &[b"counts_sum"],
                key.as_slice(),
                Element::new_item(format!("v_{}", i).into_bytes()),
                None,
                None,
                v,
            )
            .unwrap()
            .expect("insert item");
        }

        let mut q = Query::new();
        q.insert_range_inclusive(b"a".to_vec()..=b"o".to_vec());
        let proved = round_trip_offset(&db, vec![b"counts_sum".to_vec()], q, Some(3), Some(5), v);
        assert_eq!(
            proved_keys(&proved),
            vec![b"f".to_vec(), b"g".to_vec(), b"h".to_vec()],
            "ProvableCountSumTree: offset 5 + limit 3 ascending should return f,g,h"
        );
    }

    #[test]
    fn end_to_end_offset_rejects_against_non_count_tree() {
        // Sanity: the syntactic gate accepts the query (single range,
        // no subqueries, offset > 0), but the leaf merk is a NormalTree
        // — the prover's leaf-level tree-type check should fire and
        // return InvalidQuery.
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            &[] as &[&[u8]],
            b"plain",
            Element::empty_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert tree");
        for i in 0..5u8 {
            let key = vec![b'a' + i];
            db.insert(
                &[b"plain"],
                key.as_slice(),
                Element::new_item(vec![i]),
                None,
                None,
                v,
            )
            .unwrap()
            .expect("insert");
        }

        let mut q = Query::new();
        q.insert_range_inclusive(b"a".to_vec()..=b"e".to_vec());
        let path_query = PathQuery::new(
            vec![b"plain".to_vec()],
            SizedQuery::new(q, Some(3), Some(1)),
        );
        let result = db.prove_query(&path_query, None, v).unwrap();
        assert!(
            result.is_err(),
            "prover must reject offset against a NormalTree at leaf-open time"
        );
    }
}
