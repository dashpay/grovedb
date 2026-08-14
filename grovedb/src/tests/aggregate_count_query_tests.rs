//! End-to-end GroveDB tests for `AggregateCountOnRange` queries.
//!
//! These exercise the full prove → encode → decode → verify pipeline against
//! both `ProvableCountTree` and `ProvableCountSumTree` at various path
//! depths and across the full set of allowed range variants.
//!
//! NonCounted/NotCountedOrSummed wrappers are NOT accepted as children of
//! Provable* count parents (the count is cryptographically committed; see
//! `TreeType::accepts_non_counted_children`). The dedicated rejection
//! coverage lives in `non_counted_tests.rs` /
//! `not_counted_or_summed_tests.rs`.

#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::query::QueryItem;
    use grovedb_version::version::{v2::GROVE_V2, GroveVersion};

    use crate::{
        tests::{make_test_grovedb, TEST_LEAF},
        Element, GroveDb, PathQuery, SizedQuery,
    };

    /// Insert the 15 single-byte keys "a".."o" into a `ProvableCountTree`
    /// rooted at `[TEST_LEAF, "ct"]`. Returns the GroveDB and the resulting
    /// root hash.
    fn setup_15_key_provable_count_tree(
        grove_version: &GroveVersion,
    ) -> (crate::tests::TempGroveDb, [u8; 32]) {
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"ct",
            Element::empty_provable_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert ct");
        for c in b'a'..=b'o' {
            db.insert(
                [TEST_LEAF, b"ct"].as_ref(),
                &[c],
                Element::new_item(vec![c]),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert leaf");
        }
        let root = db
            .grove_db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root_hash");
        (db, root)
    }

    fn setup_15_key_provable_count_sum_tree(
        grove_version: &GroveVersion,
    ) -> (crate::tests::TempGroveDb, [u8; 32]) {
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cst",
            Element::empty_provable_count_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert cst");
        for c in b'a'..=b'o' {
            db.insert(
                [TEST_LEAF, b"cst"].as_ref(),
                &[c],
                // `Item` plays the role of a non-sum element inside a count
                // sum tree — we're testing count semantics, not sum.
                Element::new_item(vec![c]),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert leaf");
        }
        let root = db
            .grove_db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root_hash");
        (db, root)
    }

    /// Round-trip helper: build a path_query, prove it, verify it, assert
    /// `(root, count)` matches what we expect.
    fn round_trip(
        db: &crate::tests::TempGroveDb,
        expected_root: [u8; 32],
        path: Vec<Vec<u8>>,
        inner_range: QueryItem,
        expected_count: u64,
        grove_version: &GroveVersion,
    ) {
        let path_query = PathQuery::new_aggregate_count_on_range(path, inner_range);
        let proof = db
            .grove_db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove_query should succeed");
        let (root, count) =
            GroveDb::verify_aggregate_count_query(&proof, &path_query, grove_version)
                .expect("verify should succeed");
        assert_eq!(root, expected_root, "verifier reconstructed wrong root");
        assert_eq!(count, expected_count, "verifier returned wrong count");
    }

    #[test]
    fn provable_count_tree_range_inclusive() {
        let v = GroveVersion::latest();
        let (db, root) = setup_15_key_provable_count_tree(v);
        round_trip(
            &db,
            root,
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
            10,
            v,
        );
    }

    #[test]
    fn provable_count_tree_range_exclusive() {
        let v = GroveVersion::latest();
        let (db, root) = setup_15_key_provable_count_tree(v);
        round_trip(
            &db,
            root,
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::Range(b"c".to_vec()..b"l".to_vec()),
            9,
            v,
        );
    }

    #[test]
    fn provable_count_tree_range_from() {
        let v = GroveVersion::latest();
        let (db, root) = setup_15_key_provable_count_tree(v);
        round_trip(
            &db,
            root,
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeFrom(b"c".to_vec()..),
            13,
            v,
        );
    }

    #[test]
    fn provable_count_tree_range_after() {
        let v = GroveVersion::latest();
        let (db, root) = setup_15_key_provable_count_tree(v);
        round_trip(
            &db,
            root,
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeAfter(b"b".to_vec()..),
            13,
            v,
        );
    }

    #[test]
    fn provable_count_tree_range_to_inclusive() {
        let v = GroveVersion::latest();
        let (db, root) = setup_15_key_provable_count_tree(v);
        round_trip(
            &db,
            root,
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeToInclusive(..=b"e".to_vec()),
            5,
            v,
        );
    }

    #[test]
    fn provable_count_tree_range_below_all() {
        let v = GroveVersion::latest();
        let (db, root) = setup_15_key_provable_count_tree(v);
        round_trip(
            &db,
            root,
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeInclusive(vec![0x00]..=vec![0x10]),
            0,
            v,
        );
    }

    #[test]
    fn provable_count_sum_tree_range_inclusive() {
        let v = GroveVersion::latest();
        let (db, root) = setup_15_key_provable_count_sum_tree(v);
        round_trip(
            &db,
            root,
            vec![TEST_LEAF.to_vec(), b"cst".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
            10,
            v,
        );
    }

    #[test]
    fn rejects_invalid_range_at_construction() {
        // A path-query with an inner Key item should be rejected at
        // validation time, before any proof generation runs.
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::Key(b"c".to_vec()),
        );
        let err = path_query.validate_aggregate_count_on_range();
        assert!(err.is_err(), "Key inner should be rejected");
    }

    #[test]
    fn rejects_inner_range_full() {
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeFull(std::ops::RangeFull),
        );
        assert!(path_query.validate_aggregate_count_on_range().is_err());
    }

    #[test]
    fn rejects_against_normal_tree() {
        // Querying a NormalTree with AggregateCountOnRange should fail at
        // proof time with an InvalidProofError from the merk layer. We need
        // at least one element in the target normal tree so that the
        // multi-layer proof generator actually recurses into it (empty
        // trees are returned as result rows without a lower-layer descent).
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"x",
            Element::new_item(b"y".to_vec()),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("seed normal tree");
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec()],
            QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
        );
        let proof_result = db.grove_db.prove_query(&path_query, None, v).unwrap();
        assert!(
            proof_result.is_err(),
            "expected prove_query to fail on NormalTree, got {:?}",
            proof_result.ok().map(|b| b.len())
        );
    }

    #[test]
    fn count_forgery_is_caught_at_grovedb_level() {
        // End-to-end version of the merk-level forgery test: parse the
        // GroveDB envelope, descend to the leaf merk proof, find a real
        // HashWithCount op at a true op boundary, bump its count, re-encode
        // — and the GroveDB verifier should reject the resulting proof
        // (root mismatch in the layer chain).
        //
        // We parse rather than scan-for-byte to ensure we are mutating an
        // actual count varint and not, say, a 0x1e byte that happens to live
        // inside one of the embedded 32-byte hashes.
        let v = GroveVersion::latest();
        let (db, _expected_root) = setup_15_key_provable_count_tree(v);
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");

        let tampered = tamper_leaf_count(&proof, &path_query)
            .expect("expected at least one HashWithCount in the leaf merk proof");

        let verify_result = GroveDb::verify_aggregate_count_query(&tampered, &path_query, v);
        assert!(
            verify_result.is_err(),
            "tampered count must be rejected at the GroveDB verifier level, got {:?}",
            verify_result.map(|(_, c)| c)
        );
    }

    /// Decode the GroveDB proof envelope, walk down to the leaf merk proof
    /// bytes (V0: `MerkOnlyLayerProof`; V1: `LayerProof` with
    /// `ProofBytes::Merk`), parse the merk proof into ops at true op
    /// boundaries, increment the `count` of the first `HashWithCount` op,
    /// and re-encode the whole envelope.
    ///
    /// Returns `None` if no `HashWithCount` is present in the leaf merk
    /// proof — the test treats that as an invalid precondition.
    fn tamper_leaf_count(proof: &[u8], path_query: &PathQuery) -> Option<Vec<u8>> {
        use bincode::config;
        use grovedb_merk::proofs::{encoding::encode_into, Decoder, Node, Op};

        use crate::operations::proof::{
            GroveDBProof, GroveDBProofV0, GroveDBProofV1, LayerProof, MerkOnlyLayerProof,
            ProofBytes,
        };

        let cfg = config::standard()
            .with_big_endian()
            .with_limit::<{ 256 * 1024 * 1024 }>();
        let (mut decoded, _): (GroveDBProof, _) = bincode::decode_from_slice(proof, cfg).ok()?;

        // Descend through the path layers to obtain a mutable ref to the
        // leaf merk proof bytes.
        let leaf_bytes: &mut Vec<u8> = match &mut decoded {
            GroveDBProof::V0(GroveDBProofV0 { root_layer, .. }) => {
                let mut layer: &mut MerkOnlyLayerProof = root_layer;
                for key in &path_query.path {
                    layer = layer.lower_layers.get_mut(key)?;
                }
                &mut layer.merk_proof
            }
            GroveDBProof::V1(GroveDBProofV1 { root_layer }) => {
                let mut layer: &mut LayerProof = root_layer;
                for key in &path_query.path {
                    layer = layer.lower_layers.get_mut(key)?;
                }
                match &mut layer.merk_proof {
                    ProofBytes::Merk(b) => b,
                    _ => return None,
                }
            }
        };

        // Parse the merk proof into ops, mutate the first HashWithCount,
        // re-encode.
        let mut ops: Vec<Op> = Vec::new();
        for op in Decoder::new(leaf_bytes) {
            ops.push(op.ok()?);
        }

        let mut tampered = false;
        for op in ops.iter_mut() {
            match op {
                Op::Push(Node::HashWithCount(_, _, _, count))
                | Op::PushInverted(Node::HashWithCount(_, _, _, count)) => {
                    *count = count.wrapping_add(1);
                    tampered = true;
                    break;
                }
                _ => {}
            }
        }
        if !tampered {
            return None;
        }

        let mut new_leaf = Vec::new();
        encode_into(ops.iter(), &mut new_leaf);
        *leaf_bytes = new_leaf;

        bincode::encode_to_vec(
            decoded,
            config::standard().with_big_endian().with_no_limit(),
        )
        .ok()
    }

    /// Build a 3-layer path: TEST_LEAF -> "outer" (NormalTree) ->
    /// "inner" (ProvableCountTree) populated with 5 keys "a".."e".
    fn setup_three_layer_provable_count_tree(
        grove_version: &GroveVersion,
    ) -> (crate::tests::TempGroveDb, [u8; 32]) {
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert outer");
        db.insert(
            [TEST_LEAF, b"outer"].as_ref(),
            b"inner",
            Element::empty_provable_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert inner");
        for c in b'a'..=b'e' {
            db.insert(
                [TEST_LEAF, b"outer", b"inner"].as_ref(),
                &[c],
                Element::new_item(vec![c]),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert leaf");
        }
        let root = db
            .grove_db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root_hash");
        (db, root)
    }

    #[test]
    fn three_layer_path_round_trip() {
        // Exercises the multi-layer chain enforcement: layer 0 proves TEST_LEAF
        // exists, layer 1 proves "outer" exists in TEST_LEAF, layer 2 proves
        // "inner" exists in outer, layer 3 is the count proof on inner.
        let v = GroveVersion::latest();
        let (db, root) = setup_three_layer_provable_count_tree(v);
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"outer".to_vec(), b"inner".to_vec()],
            QueryItem::RangeInclusive(b"b".to_vec()..=b"d".to_vec()),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");
        let (got_root, got_count) = GroveDb::verify_aggregate_count_query(&proof, &path_query, v)
            .expect("verify should succeed");
        assert_eq!(got_root, root, "verifier root must match GroveDB root");
        assert_eq!(got_count, 3, "expected count of {{b, c, d}}");
    }

    /// Helper for non-leaf-layer proof mutation tests: decode the V1
    /// envelope, walk to the TEST_LEAF non-leaf merk proof bytes, run
    /// `mutate` over its parsed ops, re-encode the merk proof and the
    /// envelope. Returns the mutated bytes.
    fn mutate_test_leaf_layer_ops(
        proof: &[u8],
        mutate: impl FnOnce(&mut Vec<grovedb_merk::proofs::Op>),
    ) -> Vec<u8> {
        use grovedb_merk::proofs::{encoding::encode_into, Decoder, Op};

        use crate::operations::proof::{GroveDBProof, GroveDBProofV1, ProofBytes};

        let mut decoded = decode_envelope(proof);
        let GroveDBProof::V1(GroveDBProofV1 { root_layer }) = &mut decoded else {
            panic!("expected V1 envelope");
        };
        let test_leaf_layer = root_layer
            .lower_layers
            .get_mut(&TEST_LEAF.to_vec())
            .expect("TEST_LEAF lower layer");
        let bytes = match &mut test_leaf_layer.merk_proof {
            ProofBytes::Merk(b) => b,
            _ => panic!("expected Merk bytes at TEST_LEAF non-leaf"),
        };
        let mut ops: Vec<Op> = Decoder::new(bytes)
            .map(|r| r.expect("decode existing op"))
            .collect();
        mutate(&mut ops);
        let mut new_bytes = Vec::new();
        encode_into(ops.iter(), &mut new_bytes);
        *bytes = new_bytes;
        reencode_envelope(decoded)
    }

    #[test]
    fn non_leaf_proof_without_target_key_is_rejected() {
        // Mutate the TEST_LEAF non-leaf proof: replace the KV op carrying
        // the "ct" key with a Hash op carrying that node's hash. Phase 1
        // decodes successfully, the merk single-key verifier returns Ok
        // with an empty result_set (no KV with matching key), and the
        // GroveDB-level verifier surfaces "did not contain the expected
        // key" via the `ok_or_else` arm.
        use grovedb_merk::proofs::{Node, Op};

        let v = GroveVersion::latest();
        let (db, _root) = setup_15_key_provable_count_tree(v);
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");
        let mutated = mutate_test_leaf_layer_ops(&proof, |ops| {
            for op in ops.iter_mut() {
                let key_match = matches!(
                    op,
                    Op::Push(
                        Node::KV(k, _)
                        | Node::KVValueHash(k, _, _)
                        | Node::KVValueHashFeatureType(k, _, _, _)
                        | Node::KVValueHashFeatureTypeWithChildHash(k, _, _, _, _)
                    )
                    | Op::PushInverted(
                        Node::KV(k, _)
                        | Node::KVValueHash(k, _, _)
                        | Node::KVValueHashFeatureType(k, _, _, _)
                        | Node::KVValueHashFeatureTypeWithChildHash(k, _, _, _, _)
                    ) if k == b"ct"
                );
                if key_match {
                    *op = Op::Push(Node::Hash([0u8; 32]));
                    return;
                }
            }
            panic!("test setup: no `ct` KV op found in non-leaf proof");
        });
        let err = GroveDb::verify_aggregate_count_query(&mutated, &path_query, v)
            .expect_err("missing target key in non-leaf proof must be rejected");
        match err {
            crate::Error::InvalidProof(_, msg) => assert!(
                // Either Phase 2 catches "did not contain the expected key"
                // or the upstream merk single-key verifier fails first
                // because the swapped Hash makes the proof invalid; either
                // outcome closes the surface.
                msg.contains("did not contain the expected key")
                    || msg.contains("non-leaf single-key proof"),
                "unexpected message: {msg}"
            ),
            other => panic!("expected InvalidProof, got {:?}", other),
        }
    }

    #[test]
    fn non_leaf_proof_with_kv_replaced_by_kvdigest_is_rejected() {
        // Replace "ct" KV in the non-leaf proof with a KVDigest variant
        // (key + value_hash, no value). The result_set will contain "ct"
        // but with `value = None`, hitting the "no value bytes" arm of
        // `verify_single_key_layer_proof_v0`.
        use grovedb_merk::proofs::{Node, Op};

        let v = GroveVersion::latest();
        let (db, _root) = setup_15_key_provable_count_tree(v);
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");
        let mutated = mutate_test_leaf_layer_ops(&proof, |ops| {
            for op in ops.iter_mut() {
                let replaced = match op {
                    Op::Push(Node::KVValueHash(k, _, vh))
                    | Op::PushInverted(Node::KVValueHash(k, _, vh))
                        if k == b"ct" =>
                    {
                        Some((k.clone(), *vh))
                    }
                    Op::Push(Node::KVValueHashFeatureType(k, _, vh, _))
                    | Op::PushInverted(Node::KVValueHashFeatureType(k, _, vh, _))
                        if k == b"ct" =>
                    {
                        Some((k.clone(), *vh))
                    }
                    Op::Push(Node::KVValueHashFeatureTypeWithChildHash(k, _, vh, _, _))
                    | Op::PushInverted(Node::KVValueHashFeatureTypeWithChildHash(k, _, vh, _, _))
                        if k == b"ct" =>
                    {
                        Some((k.clone(), *vh))
                    }
                    _ => None,
                };
                if let Some((k, vh)) = replaced {
                    *op = Op::Push(Node::KVDigest(k, vh));
                    return;
                }
            }
            panic!("test setup: no `ct` KVValueHash-flavored op found in non-leaf proof");
        });
        let result = GroveDb::verify_aggregate_count_query(&mutated, &path_query, v);
        // Either we hit the "no value bytes" arm (line 295-302) or the
        // merk single-key verifier itself rejects the type swap. Both
        // are valid — both close the attack surface.
        match result {
            Err(crate::Error::InvalidProof(_, _)) => {}
            other => panic!("expected InvalidProof, got {:?}", other),
        }
    }

    #[test]
    fn non_leaf_proof_with_undeserializable_value_is_rejected() {
        // Mutate the "ct" KV node's value bytes to garbage that fails
        // `Element::deserialize`. The merk single-key verifier still
        // returns Ok (it just hashes the bytes — it doesn't deserialize),
        // so enforce_lower_chain hits the deserialize-failure arm.
        use grovedb_merk::proofs::{Node, Op};

        let v = GroveVersion::latest();
        let (db, _root) = setup_15_key_provable_count_tree(v);
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");
        // Garbage that no Element variant tag matches.
        let garbage: Vec<u8> = vec![0xff, 0xff, 0xff];
        let mutated = mutate_test_leaf_layer_ops(&proof, |ops| {
            for op in ops.iter_mut() {
                let replaced = match op {
                    Op::Push(Node::KVValueHash(k, val, _))
                    | Op::PushInverted(Node::KVValueHash(k, val, _))
                        if k == b"ct" =>
                    {
                        *val = garbage.clone();
                        true
                    }
                    Op::Push(Node::KVValueHashFeatureType(k, val, _, _))
                    | Op::PushInverted(Node::KVValueHashFeatureType(k, val, _, _))
                        if k == b"ct" =>
                    {
                        *val = garbage.clone();
                        true
                    }
                    Op::Push(Node::KVValueHashFeatureTypeWithChildHash(k, val, _, _, _))
                    | Op::PushInverted(Node::KVValueHashFeatureTypeWithChildHash(
                        k,
                        val,
                        _,
                        _,
                        _,
                    )) if k == b"ct" => {
                        *val = garbage.clone();
                        true
                    }
                    _ => false,
                };
                if replaced {
                    return;
                }
            }
            panic!("test setup: no `ct` value-bearing KV op found in non-leaf proof");
        });
        let result = GroveDb::verify_aggregate_count_query(&mutated, &path_query, v);
        // Either the deserialize arm fires (line 330-338) or the chain
        // mismatch fires first (because mutating value bytes also breaks
        // the value_hash binding committed by the parent). Either rejects.
        assert!(
            matches!(result, Err(crate::Error::InvalidProof(_, _))),
            "mutated value bytes must be rejected, got {:?}",
            result.map(|(_, c)| c)
        );
    }

    #[test]
    fn non_leaf_proof_with_non_tree_element_is_rejected() {
        // Mutate the "ct" value bytes to a serialized non-tree Element
        // (Item). This deserializes successfully, but enforce_lower_chain's
        // `is_any_tree()` guard rejects: aggregate-count proofs can only
        // descend through tree elements.
        use grovedb_merk::proofs::{Node, Op};

        let v = GroveVersion::latest();
        let (db, _root) = setup_15_key_provable_count_tree(v);
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");
        let item_bytes = Element::new_item(vec![0xab, 0xcd])
            .serialize(v)
            .expect("serialize item");
        let mutated = mutate_test_leaf_layer_ops(&proof, |ops| {
            for op in ops.iter_mut() {
                let replaced = match op {
                    Op::Push(Node::KVValueHash(k, val, _))
                    | Op::PushInverted(Node::KVValueHash(k, val, _))
                        if k == b"ct" =>
                    {
                        *val = item_bytes.clone();
                        true
                    }
                    Op::Push(Node::KVValueHashFeatureType(k, val, _, _))
                    | Op::PushInverted(Node::KVValueHashFeatureType(k, val, _, _))
                        if k == b"ct" =>
                    {
                        *val = item_bytes.clone();
                        true
                    }
                    Op::Push(Node::KVValueHashFeatureTypeWithChildHash(k, val, _, _, _))
                    | Op::PushInverted(Node::KVValueHashFeatureTypeWithChildHash(
                        k,
                        val,
                        _,
                        _,
                        _,
                    )) if k == b"ct" => {
                        *val = item_bytes.clone();
                        true
                    }
                    _ => false,
                };
                if replaced {
                    return;
                }
            }
            panic!("test setup: no `ct` value-bearing KV op found in non-leaf proof");
        });
        let result = GroveDb::verify_aggregate_count_query(&mutated, &path_query, v);
        // Either the non-tree branch fires (line 341-349) or the chain
        // hash check fails first (value_hash for the swapped item bytes
        // diverges from the parent's commitment). Either rejects.
        assert!(
            matches!(result, Err(crate::Error::InvalidProof(_, _))),
            "non-tree element on path must be rejected, got {:?}",
            result.map(|(_, c)| c)
        );
    }

    #[test]
    fn aggregate_count_with_missing_path_and_invalid_inner_is_rejected_at_entry() {
        // Codex finding: validation only fires inside `prove_subqueries` when
        // the recursion reaches the aggregate-count-bearing leaf level. If the path
        // doesn't exist (e.g. "missing" key under TEST_LEAF), the recursive
        // prover never sees the aggregate-count item and the malformed query is allowed
        // to return a regular path/absence proof. Fix: validate at the
        // `prove_query` entry point, before any recursive dispatch.
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"missing".to_vec()],
            // QueryItem::Key as the inner range is invalid for aggregate-count.
            QueryItem::Key(b"k".to_vec()),
        );
        let prove_result = db.grove_db.prove_query(&path_query, None, v).unwrap();
        match prove_result {
            Err(crate::Error::InvalidQuery(msg)) => {
                assert!(
                    msg.contains("AggregateCountOnRange may not wrap Key"),
                    "expected `AggregateCountOnRange`-Key rejection, got: {msg}"
                );
            }
            other => panic!(
                "malformed aggregate-count with non-existent path must be rejected at entry, got {:?}",
                other.map(|b| b.len())
            ),
        }
    }

    #[test]
    fn aggregate_count_hidden_in_subquery_branch_with_invalid_inner_is_rejected_at_entry() {
        // After the carrier aggregate-count feature landed, an `AggregateCountOnRange`
        // smuggled inside a `default_subquery_branch.subquery` is **valid**
        // when the surrounding query satisfies the carrier rules — that is
        // the whole point of the carrier shape.
        //
        // What this test still guards is the *other* malformed case: a
        // carrier whose subquery is itself a malformed leaf `AggregateCountOnRange` (here, an
        // aggregate-count wrapping `Key` — leaf rule 3). The carrier validator
        // delegates to `validate_leaf_aggregate_count_on_range`, which
        // surfaces the malformed-inner error, and the prove-entry gate
        // refuses to run the query.
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        let bad_inner_aggregate_count =
            QueryItem::AggregateCountOnRange(Box::new(QueryItem::Key(b"k".to_vec())));
        let mut sub_query = grovedb_merk::proofs::Query::new();
        sub_query.insert_item(bad_inner_aggregate_count);
        let mut top_query = grovedb_merk::proofs::Query::new();
        top_query.insert_range_inclusive(b"a".to_vec()..=b"z".to_vec());
        top_query.set_subquery(sub_query);
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec()],
            SizedQuery::new(top_query, None, None),
        );
        let prove_result = db.grove_db.prove_query(&path_query, None, v).unwrap();
        // Pin the specific reason (the leaf validator's "wrap Key"
        // rejection delegated through the carrier validator) so a
        // future refactor that re-routes the rejection through a
        // different but still-`InvalidQuery` arm doesn't silently
        // accept the malformed shape.
        match prove_result {
            Err(crate::Error::InvalidQuery(msg)) => assert!(
                msg.contains("AggregateCountOnRange may not wrap Key"),
                "expected malformed-inner-Key rejection, got: {msg}"
            ),
            other => panic!(
                "carrier aggregate-count with malformed leaf-inner Key must be rejected at entry, got {:?}",
                other.map(|b| b.len())
            ),
        }
    }

    #[test]
    fn corrupted_path_layer_byte_is_rejected() {
        // Tamper with a non-leaf-layer byte (a tree-element value byte) and
        // verify that the chain enforcement catches it. We pick a byte deep
        // enough that it lands inside one of the parent merk's KV value bytes.
        let v = GroveVersion::latest();
        let (db, _root) = setup_three_layer_provable_count_tree(v);
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"outer".to_vec(), b"inner".to_vec()],
            QueryItem::RangeInclusive(b"b".to_vec()..=b"d".to_vec()),
        );
        let mut proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");
        // Flip a byte well inside the proof — the exact location doesn't
        // matter as long as it isn't the bincode envelope length prefix.
        // Index 32 is past the envelope and into the first inner merk's bytes.
        let target = proof.len() / 2;
        proof[target] = proof[target].wrapping_add(1);
        let verify_result = GroveDb::verify_aggregate_count_query(&proof, &path_query, v);
        assert!(
            verify_result.is_err(),
            "tampered proof byte must be rejected, got {:?}",
            verify_result.map(|(_, c)| c)
        );
    }

    #[test]
    fn aggregate_count_rejects_grove_v2_envelope() {
        // GROVE_V2 dispatches to the V0 prove_query_non_serialized path,
        // which produces a `MerkOnlyLayerProof` envelope. aggregate-count was added
        // after V0 envelopes were superseded by V1 (in the grove version
        // used by Dash Platform v12+), so V0+aggregate-count is impossible in any
        // deployed Platform release. The prover rejects the combination
        // up front to keep callers from emitting a V0 aggregate-count proof that
        // the verifier would (correctly) refuse.
        let v: &GroveVersion = &GROVE_V2;
        let (db, _root) = setup_15_key_provable_count_tree(v);
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
        );
        let prove_result = db.grove_db.prove_query(&path_query, None, v).unwrap();
        match prove_result {
            Err(crate::Error::NotSupported(msg)) => assert!(
                msg.contains("V1 proof envelopes"),
                "unexpected message: {msg}"
            ),
            other => panic!(
                "expected NotSupported for V0+aggregate-count, got {:?}",
                other.map(|b| b.len())
            ),
        }
    }

    #[test]
    fn verify_rejects_malformed_path_query_at_entry() {
        // Even before any proof bytes are decoded, the verifier rejects a
        // path_query that isn't a well-formed AggregateCountOnRange query.
        let v = GroveVersion::latest();
        let bad_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec()],
            QueryItem::Key(b"k".to_vec()), // inner Key is not allowed
        );
        // Any proof bytes are fine — validation happens before decoding.
        let dummy_proof = vec![0u8; 16];
        let err = GroveDb::verify_aggregate_count_query(&dummy_proof, &bad_query, v)
            .expect_err("malformed path_query must be rejected up front");
        let s = format!("{:?}", err);
        assert!(
            s.contains("Key") || s.contains("InvalidQuery"),
            "got: {}",
            s
        );
    }

    #[test]
    fn validate_at_construction_rejects_nested_aggregate_count_on_range() {
        // Nested AggregateCountOnRange is rejected at validation time.
        let pq = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::AggregateCountOnRange(Box::new(QueryItem::Range(
                b"a".to_vec()..b"z".to_vec(),
            ))),
        );
        assert!(pq.validate_aggregate_count_on_range().is_err());
    }

    /// `Element::NonCounted` wrappers are NOT accepted as children of
    /// `ProvableCountTree` (or any other `Provable*` count parent).
    /// The aggregate count baked into every node hash via
    /// `node_hash_with_count` IS the answer to an
    /// `AggregateCountOnRange` query, and an opt-out from that count
    /// would commit a cryptographic value that disagrees with the
    /// actual number of stored elements. The merk-layer insert guard
    /// rejects the wrapper before any data lands.
    ///
    /// This test pins the rejection so the `AggregateCountOnRange`
    /// query never has to reason about a "NonCounted leaf in a
    /// ProvableCountTree" shape — the shape is impossible to construct
    /// going forward.
    #[test]
    fn non_counted_child_rejected_in_provable_count_tree() {
        use crate::tests::TEST_LEAF;

        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"ct",
            Element::empty_provable_count_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert ct");

        let nc_item =
            Element::new_non_counted(Element::new_item(b"hidden".to_vec())).expect("wrap ok");
        let err = db
            .insert([TEST_LEAF, b"ct"].as_ref(), b"f", nc_item, None, None, v)
            .unwrap()
            .expect_err("insert NonCounted into ProvableCountTree must fail");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("non-counted"),
            "expected NonCounted parent-type guard error, got: {msg}"
        );
    }

    /// Pin observable cost numbers + proof byte size for a known input so
    /// regressions in the proof shape (extra unnecessary nodes, missing
    /// short-circuit, etc.) show up as a test failure instead of as a
    /// silent perf hit. Values are exact for the 15-key
    /// `ProvableCountTree` + `RangeInclusive("c"..="l")` setup; if the
    /// proof shape changes intentionally, update them here.
    #[test]
    fn proof_size_snapshot_for_15_key_closed_range() {
        let v = GroveVersion::latest();
        let (db, _root) = setup_15_key_provable_count_tree(v);
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove");

        // Snapshot the proof byte size. The current shape produces a small
        // deterministic byte stream; if this drifts upward without
        // intent, the proof shape may have regressed.
        //
        // The acceptable range is conservative — we only require the
        // proof stays bounded by what an O(log n) shape predicts for a
        // 4-level tree (a few hundred bytes is the right ballpark; many
        // KB would indicate the count short-circuit didn't fire). The
        // *current* size is around 650 bytes; a few hundred bytes of
        // headroom in either direction tolerates encoding tweaks but
        // catches gross regressions.
        let len = proof.len();
        assert!(
            (300..=900).contains(&len),
            "aggregate-count proof size {} bytes is outside the expected \
             [300, 900] window for a 15-key 2-layer query — proof shape \
             may have regressed",
            len
        );

        // Round-trip through the verifier as a sanity check that the
        // pinned shape is still verifiable.
        let (_root, count) =
            GroveDb::verify_aggregate_count_query(&proof, &path_query, v).expect("verify");
        assert_eq!(count, 10);
    }

    /// Re-encode a (possibly mutated) `GroveDBProof` envelope using the same
    /// bincode config the prover uses on the way out.
    fn reencode_envelope(decoded: crate::operations::proof::GroveDBProof) -> Vec<u8> {
        bincode::encode_to_vec(
            decoded,
            bincode::config::standard()
                .with_big_endian()
                .with_no_limit(),
        )
        .expect("re-encode envelope")
    }

    fn decode_envelope(proof: &[u8]) -> crate::operations::proof::GroveDBProof {
        bincode::decode_from_slice(
            proof,
            bincode::config::standard()
                .with_big_endian()
                .with_limit::<{ 256 * 1024 * 1024 }>(),
        )
        .expect("decode envelope")
        .0
    }

    #[test]
    fn v1_envelope_with_non_merk_proof_bytes_is_rejected() {
        // The verifier's V1 layer walker only accepts `ProofBytes::Merk(_)`
        // for aggregate-count proofs (other tree types — MMR / BulkAppend /
        // Dense / CommitmentTree — cannot host provable count subtrees). If
        // we swap the leaf layer's bytes for an `MMR(_)` variant, verification
        // must fail with an `InvalidProof` error rather than silently
        // succeed or panic.
        use crate::operations::proof::{GroveDBProof, GroveDBProofV1, ProofBytes};

        let v = GroveVersion::latest();
        let (db, _root) = setup_15_key_provable_count_tree(v);
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");

        let mut decoded = decode_envelope(&proof);
        let GroveDBProof::V1(GroveDBProofV1 { root_layer }) = &mut decoded else {
            panic!("expected V1 envelope on latest GroveVersion");
        };

        // Walk to the leaf layer (depth = path.len()) and swap its bytes
        // for an MMR variant.
        let leaf_layer = root_layer
            .lower_layers
            .get_mut(&TEST_LEAF.to_vec())
            .expect("TEST_LEAF lower layer")
            .lower_layers
            .get_mut(&b"ct".to_vec())
            .expect("ct lower layer");
        leaf_layer.merk_proof = ProofBytes::MMR(vec![0u8; 8]);

        let reencoded = reencode_envelope(decoded);
        let err = GroveDb::verify_aggregate_count_query(&reencoded, &path_query, v)
            .expect_err("non-Merk leaf bytes must be rejected");
        match err {
            crate::Error::InvalidProof(_, msg) => {
                assert!(
                    msg.contains("non-merk"),
                    "expected non-merk rejection, got: {msg}"
                );
            }
            other => panic!("expected InvalidProof, got {:?}", other),
        }
    }

    #[test]
    fn v1_envelope_with_missing_lower_layer_is_rejected() {
        // The verifier expects a `lower_layers` entry for each non-leaf
        // path key. If the prover (or an attacker) drops one, verification
        // must fail rather than silently descend through a stub.
        use crate::operations::proof::{GroveDBProof, GroveDBProofV1};

        let v = GroveVersion::latest();
        let (db, _root) = setup_15_key_provable_count_tree(v);
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");

        let mut decoded = decode_envelope(&proof);
        let GroveDBProof::V1(GroveDBProofV1 { root_layer }) = &mut decoded else {
            panic!("expected V1 envelope on latest GroveVersion");
        };
        let test_leaf_layer = root_layer
            .lower_layers
            .get_mut(&TEST_LEAF.to_vec())
            .expect("TEST_LEAF lower layer");
        // Drop the leaf layer's pointer entry.
        let removed = test_leaf_layer.lower_layers.remove(&b"ct".to_vec());
        assert!(removed.is_some(), "test setup: ct layer should exist");

        let reencoded = reencode_envelope(decoded);
        let err = GroveDb::verify_aggregate_count_query(&reencoded, &path_query, v)
            .expect_err("missing lower_layer must be rejected");
        match err {
            crate::Error::InvalidProof(_, msg) => {
                assert!(
                    msg.contains("missing lower layer"),
                    "expected missing-lower-layer rejection, got: {msg}"
                );
            }
            other => panic!("expected InvalidProof, got {:?}", other),
        }
    }

    #[test]
    fn v1_envelope_with_malformed_leaf_count_proof_is_rejected() {
        // Replace the leaf merk proof bytes with a single Push(Hash(...))
        // op stream. Phase 1 of the count verifier rejects plain `Hash` as
        // a non-allowlisted node type, so `verify_count_leaf` surfaces an
        // `InvalidProof` error via its `.map_err(...)` arm rather than
        // ever reaching the chain check.
        use std::collections::LinkedList;

        use grovedb_merk::proofs::{encoding::encode_into, Node, Op};

        use crate::operations::proof::{GroveDBProof, GroveDBProofV1, ProofBytes};

        let v = GroveVersion::latest();
        let (db, _root) = setup_15_key_provable_count_tree(v);
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");

        let mut decoded = decode_envelope(&proof);
        let GroveDBProof::V1(GroveDBProofV1 { root_layer }) = &mut decoded else {
            panic!("expected V1 envelope");
        };
        let leaf_layer = root_layer
            .lower_layers
            .get_mut(&TEST_LEAF.to_vec())
            .expect("TEST_LEAF lower layer")
            .lower_layers
            .get_mut(&b"ct".to_vec())
            .expect("ct lower layer");

        // Build a malformed (but parseable) merk proof: a single Push(Hash)
        // that the count verifier's Phase 1 rejects.
        let mut ops: LinkedList<Op> = LinkedList::new();
        ops.push_back(Op::Push(Node::Hash([0u8; 32])));
        let mut bad_bytes = Vec::new();
        encode_into(ops.iter(), &mut bad_bytes);
        leaf_layer.merk_proof = ProofBytes::Merk(bad_bytes);

        let reencoded = reencode_envelope(decoded);
        let err = GroveDb::verify_aggregate_count_query(&reencoded, &path_query, v)
            .expect_err("malformed leaf count proof must be rejected");
        match err {
            crate::Error::InvalidProof(_, msg) => {
                assert!(
                    msg.contains("aggregate-count leaf proof failed to verify"),
                    "expected leaf-verify failure message, got: {msg}"
                );
            }
            other => panic!("expected InvalidProof, got {:?}", other),
        }
    }

    #[test]
    fn v1_envelope_with_corrupted_non_leaf_merk_bytes_is_rejected() {
        // Mutate the non-leaf merk proof bytes (the layer that proves
        // existence of the "ct" tree element under TEST_LEAF). The
        // single-key proof verification at that layer should fail before
        // we ever descend to the leaf count proof.
        use crate::operations::proof::{GroveDBProof, GroveDBProofV1, ProofBytes};

        let v = GroveVersion::latest();
        let (db, _root) = setup_15_key_provable_count_tree(v);
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");

        let mut decoded = decode_envelope(&proof);
        let GroveDBProof::V1(GroveDBProofV1 { root_layer }) = &mut decoded else {
            panic!("expected V1 envelope");
        };
        // Corrupt the TEST_LEAF non-leaf merk proof bytes by truncating to
        // a 1-byte payload, which fails to decode as a proof op stream.
        let test_leaf_layer = root_layer
            .lower_layers
            .get_mut(&TEST_LEAF.to_vec())
            .expect("TEST_LEAF lower layer");
        match &mut test_leaf_layer.merk_proof {
            ProofBytes::Merk(b) => {
                *b = vec![0xff];
            }
            other => panic!(
                "expected Merk bytes at non-leaf, got discriminant {:?}",
                std::mem::discriminant(other)
            ),
        }

        let reencoded = reencode_envelope(decoded);
        let err = GroveDb::verify_aggregate_count_query(&reencoded, &path_query, v)
            .expect_err("corrupted non-leaf merk bytes must be rejected");
        match err {
            crate::Error::InvalidProof(_, _) => {}
            other => panic!("expected InvalidProof, got {:?}", other),
        }
    }

    /// Security regression: empty-path aggregate-count queries are
    /// rejected at validation time, before any proof handling.
    ///
    /// `verify_aggregate_count_query` calls
    /// `path_query.validate_aggregate_count_on_range()` at its entry. If
    /// the path is empty, validation must fail — otherwise both
    /// `verify_v0_layer` and `verify_v1_layer` would hit the
    /// `depth == path_keys.len()` short-circuit at depth 0 and go
    /// straight to the merk-level leaf verifier, never invoking the
    /// terminal-type gate in `enforce_lower_chain`. The GroveDB root
    /// merk is always a `NormalTree` by API construction, so a root
    /// aggregate-count query has no valid target.
    #[test]
    fn empty_path_aggregate_count_rejected_at_validation() {
        let v = GroveVersion::latest();
        let pq = PathQuery::new_aggregate_count_on_range(
            Vec::new(),
            QueryItem::RangeFrom(b"a".to_vec()..),
        );
        let err = pq
            .validate_aggregate_count_on_range()
            .expect_err("empty path must be rejected at validation");
        let msg = format!("{err}");
        assert!(
            msg.contains("root")
                && (msg.contains("ProvableCountTree") || msg.contains("ProvableCountSumTree")),
            "expected message naming root + ProvableCountTree, got: {msg}"
        );

        let result = GroveDb::verify_aggregate_count_query(&[0u8; 4], &pq, v);
        assert!(
            result.is_err(),
            "verify_aggregate_count_query must reject empty-path queries"
        );
    }

    /// Security regression: empty-leaf type-confusion forgery
    /// (parallel of `empty_leaf_type_confusion_forgery_rejected` on the
    /// sum side).
    ///
    /// The honest leaf is an empty NormalTree (root_key=None). Every
    /// empty Merk-backed tree stores `inner_root = NULL_HASH`, so its
    /// recorded value_hash equals `combine_hash(H(element_bytes),
    /// NULL_HASH)`. The merk-level count verifier accepts empty proof
    /// bytes as `(NULL_HASH, 0)`. Before the fix the verifier's loose
    /// `is_any_tree()` check happily accepted NormalTree element bytes
    /// and the chain hash matched by coincidence, letting an attacker
    /// prove `count = 0` against a path that wasn't actually a
    /// ProvableCountTree. The numeric answer (0) is correct for an
    /// empty tree of any type, but the implicit claim "the leaf is a
    /// ProvableCountTree" was a soundness gap.
    #[test]
    fn empty_leaf_type_confusion_forgery_rejected() {
        use std::collections::BTreeMap;

        use bincode::config;
        use grovedb_version::version::v2::GROVE_V2;

        use crate::operations::proof::{
            GroveDBProof, GroveDBProofV0, MerkOnlyLayerProof, ProveOptions,
        };

        // Use V0 (GROVE_V2) envelope — its MerkOnlyLayerProof is simpler
        // to surgically reconstruct than V1's LayerProof/ProofBytes.
        let v: &GroveVersion = &GROVE_V2;
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"evil",
            Element::empty_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert empty normal tree at evil");

        // Honest probe to harvest the layer-0 merk proof bytes that prove
        // `evil` exists in the TEST_LEAF merk with its NormalTree element
        // bytes.
        let probe = PathQuery::new_single_key(vec![TEST_LEAF.to_vec()], b"evil".to_vec());
        let probe_proof_bytes = db
            .grove_db
            .prove_query(&probe, None, v)
            .unwrap()
            .expect("honest probe should succeed");

        let cfg = config::standard()
            .with_big_endian()
            .with_limit::<{ 256 * 1024 * 1024 }>();
        let probe_decoded: GroveDBProof = bincode::decode_from_slice(&probe_proof_bytes, cfg)
            .unwrap()
            .0;

        let (root_mp, test_leaf_mp) = match probe_decoded {
            GroveDBProof::V0(GroveDBProofV0 { root_layer, .. }) => (
                root_layer.merk_proof,
                root_layer
                    .lower_layers
                    .get(TEST_LEAF)
                    .expect("descent")
                    .merk_proof
                    .clone(),
            ),
            GroveDBProof::V1(_) => panic!("expected V0 envelope under GROVE_V2"),
        };

        let leaf = MerkOnlyLayerProof {
            merk_proof: Vec::new(),
            lower_layers: BTreeMap::new(),
        };
        let mut test_leaf_map = BTreeMap::new();
        test_leaf_map.insert(b"evil".to_vec(), leaf);
        let test_leaf_layer = MerkOnlyLayerProof {
            merk_proof: test_leaf_mp,
            lower_layers: test_leaf_map,
        };
        let mut root_lower = BTreeMap::new();
        root_lower.insert(TEST_LEAF.to_vec(), test_leaf_layer);

        let forged = GroveDBProof::V0(GroveDBProofV0 {
            root_layer: MerkOnlyLayerProof {
                merk_proof: root_mp,
                lower_layers: root_lower,
            },
            prove_options: ProveOptions::default(),
        });
        let forged_bytes = bincode::encode_to_vec(&forged, cfg).expect("encode");

        let attack_pq = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"evil".to_vec()],
            QueryItem::RangeFrom(b"a".to_vec()..),
        );

        let result = GroveDb::verify_aggregate_count_query(&forged_bytes, &attack_pq, v);
        match result {
            Err(e) => {
                let msg = format!("{e}");
                // The forgery is rejected either by:
                //   (a) the V0-envelope-not-allowed gate (fires first
                //       under GROVE_V2), or
                //   (b) the terminal-type gate (fires under V1 envelopes
                //       if we reach it).
                // Either rejection means the forgery doesn't pass — the
                // security property holds. Accept both error shapes here.
                assert!(
                    msg.contains("must be a ProvableCountTree")
                        || msg.contains("ProvableCountSumTree")
                        || msg.contains("require V1 proof envelopes"),
                    "verifier rejected as expected but with an unrelated message: {msg}"
                );
            }
            Ok((root_hash, count)) => panic!(
                "BUG: empty-leaf forgery accepted by aggregate-count verifier! \
                 Returned (root_hash={}, count={}) — the leaf is a NormalTree, \
                 not a ProvableCountTree.",
                hex::encode(root_hash),
                count
            ),
        }
    }

    // -------------------------------------------------------------------
    // Tests for the no-proof variant: GroveDb::query_aggregate_count.
    //
    // The no-proof variant must return the same count as the proof
    // variant for every valid PathQuery shape, but should not need to
    // produce or verify any proof bytes. These tests mirror the proof
    // round-trip tests above and additionally cover the failure modes
    // unique to the no-proof path (missing path, non-provable-count
    // tree type).
    // -------------------------------------------------------------------

    /// No-proof helper: build the path-query, call query_aggregate_count,
    /// assert the returned count matches the expected value AND matches
    /// what the proof round-trip returns.
    fn no_proof_matches_proof(
        db: &crate::tests::TempGroveDb,
        path: Vec<Vec<u8>>,
        inner_range: QueryItem,
        expected_count: u64,
        grove_version: &GroveVersion,
    ) {
        let path_query = PathQuery::new_aggregate_count_on_range(path, inner_range);

        let direct = db
            .grove_db
            .query_aggregate_count(&path_query, None, grove_version)
            .unwrap()
            .expect("query_aggregate_count should succeed");
        assert_eq!(
            direct, expected_count,
            "no-proof variant returned wrong count"
        );

        let proof = db
            .grove_db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove_query should succeed");
        let (_root, proved) =
            GroveDb::verify_aggregate_count_query(&proof, &path_query, grove_version)
                .expect("verify should succeed");
        assert_eq!(
            direct, proved,
            "no-proof variant disagrees with proof variant"
        );
    }

    #[test]
    fn no_proof_provable_count_tree_range_inclusive() {
        let v = GroveVersion::latest();
        let (db, _) = setup_15_key_provable_count_tree(v);
        no_proof_matches_proof(
            &db,
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
            10,
            v,
        );
    }

    #[test]
    fn no_proof_provable_count_tree_range_exclusive() {
        let v = GroveVersion::latest();
        let (db, _) = setup_15_key_provable_count_tree(v);
        no_proof_matches_proof(
            &db,
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::Range(b"c".to_vec()..b"l".to_vec()),
            9,
            v,
        );
    }

    #[test]
    fn no_proof_provable_count_tree_range_from() {
        let v = GroveVersion::latest();
        let (db, _) = setup_15_key_provable_count_tree(v);
        no_proof_matches_proof(
            &db,
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeFrom(b"c".to_vec()..),
            13,
            v,
        );
    }

    #[test]
    fn no_proof_provable_count_tree_range_after() {
        let v = GroveVersion::latest();
        let (db, _) = setup_15_key_provable_count_tree(v);
        no_proof_matches_proof(
            &db,
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeAfter(b"b".to_vec()..),
            13,
            v,
        );
    }

    #[test]
    fn no_proof_provable_count_tree_range_to_inclusive() {
        let v = GroveVersion::latest();
        let (db, _) = setup_15_key_provable_count_tree(v);
        no_proof_matches_proof(
            &db,
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeToInclusive(..=b"e".to_vec()),
            5,
            v,
        );
    }

    #[test]
    fn no_proof_range_disjoint_from_all_keys() {
        let v = GroveVersion::latest();
        let (db, _) = setup_15_key_provable_count_tree(v);
        no_proof_matches_proof(
            &db,
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeInclusive(vec![0x00]..=vec![0x10]),
            0,
            v,
        );
    }

    #[test]
    fn no_proof_provable_count_sum_tree_range_inclusive() {
        let v = GroveVersion::latest();
        let (db, _) = setup_15_key_provable_count_sum_tree(v);
        no_proof_matches_proof(
            &db,
            vec![TEST_LEAF.to_vec(), b"cst".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
            10,
            v,
        );
    }

    #[test]
    fn no_proof_three_layer_path() {
        let v = GroveVersion::latest();
        let (db, _) = setup_three_layer_provable_count_tree(v);
        no_proof_matches_proof(
            &db,
            vec![TEST_LEAF.to_vec(), b"outer".to_vec(), b"inner".to_vec()],
            QueryItem::RangeInclusive(b"b".to_vec()..=b"d".to_vec()),
            3,
            v,
        );
    }

    #[test]
    fn no_proof_rejects_carrier_shape() {
        // `query_aggregate_count` returns a single `u64` and has no way
        // to surface per-outer-key carrier counts. Calling it with a
        // carrier-shape path query must be rejected up front by the
        // leaf-only validator, BEFORE any storage reads happen — even
        // though the dispatcher-level `validate_aggregate_count_on_range`
        // would have accepted the same query.
        use grovedb_query::Query;
        let v = GroveVersion::latest();
        let (db, _) = setup_15_key_provable_count_tree(v);

        let mut carrier = Query::new();
        carrier.insert_key(b"brand_000".to_vec());
        carrier.set_subquery_path(vec![b"color".to_vec()]);
        carrier.set_subquery(Query::new_aggregate_count_on_range(QueryItem::Range(
            b"a".to_vec()..b"z".to_vec(),
        )));
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            SizedQuery::new(carrier, None, None),
        );

        // Sanity: the dispatcher-level validator accepts this as a
        // valid carrier, so the rejection below is specifically
        // because `query_aggregate_count` tightens to leaf-only.
        assert!(path_query.validate_aggregate_count_on_range().is_ok());

        let err = db
            .grove_db
            .query_aggregate_count(&path_query, None, v)
            .unwrap()
            .expect_err("carrier shape must be rejected at the no-proof entry");
        assert!(
            matches!(err, crate::Error::InvalidQuery(_)),
            "expected InvalidQuery, got {:?}",
            err
        );
    }

    #[test]
    fn no_proof_rejects_invalid_inner_range() {
        // Same shape check the prover/verifier use: Key inner is invalid for
        // an aggregate-count-on-range query.
        let v = GroveVersion::latest();
        let (db, _) = setup_15_key_provable_count_tree(v);
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::Key(b"c".to_vec()),
        );
        let err = db
            .grove_db
            .query_aggregate_count(&path_query, None, v)
            .unwrap()
            .expect_err("Key inner must be rejected before any storage reads");
        assert!(
            matches!(err, crate::Error::InvalidQuery(_)),
            "expected InvalidQuery, got {:?}",
            err
        );
    }

    #[test]
    fn no_proof_rejects_against_normal_tree() {
        // The merk-level entry point gates on
        // `tree_type ∈ {ProvableCountTree, ProvableCountSumTree}`. A
        // NormalTree must surface that as a MerkError.
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"x",
            Element::new_item(b"y".to_vec()),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("seed normal tree");
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec()],
            QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
        );
        let err = db
            .grove_db
            .query_aggregate_count(&path_query, None, v)
            .unwrap()
            .expect_err("NormalTree must be rejected by the merk-level entry");
        // The merk-level error gets wrapped with contextual `CorruptedData`
        // (callsite-specific path info — see `query_aggregate_count` in
        // `operations/get/query.rs`). We just require *some* error rather
        // than asserting on the exact variant since the merk layer's
        // `InvalidProofError` formatting is internal.
        match err {
            crate::Error::CorruptedData(_) => {}
            other => panic!("expected CorruptedData wrapper, got {:?}", other),
        }
    }

    #[test]
    fn no_proof_uses_provided_transaction() {
        // Exercise the TransactionArg = Some(&tx) path of query_aggregate_count
        // and verify the transactional read actually observes uncommitted
        // state. The base view must NOT see the in-transaction insert.
        let v = GroveVersion::latest();
        let (db, _) = setup_15_key_provable_count_tree(v);
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
        );

        // Sanity check: base view sees 10 keys in [c, l].
        let base_count = db
            .grove_db
            .query_aggregate_count(&path_query, None, v)
            .unwrap()
            .expect("base query should succeed");
        assert_eq!(base_count, 10, "base view should see 10 keys");

        // Insert a new in-range key ("k2") inside a transaction.
        let tx = db.start_transaction();
        db.insert(
            [TEST_LEAF, b"ct"].as_ref(),
            b"k2",
            Element::new_item(b"k2".to_vec()),
            None,
            Some(&tx),
            v,
        )
        .unwrap()
        .expect("transactional insert should succeed");

        // Transactional read must include the uncommitted insert (11).
        let tx_count = db
            .grove_db
            .query_aggregate_count(&path_query, Some(&tx), v)
            .unwrap()
            .expect("transactional query should succeed");
        assert_eq!(
            tx_count, 11,
            "transactional view must include uncommitted insert"
        );

        // Base view must still see 10 — the uncommitted insert is invisible
        // to non-transactional reads.
        let base_count_after = db
            .grove_db
            .query_aggregate_count(&path_query, None, v)
            .unwrap()
            .expect("base query should succeed after tx insert");
        assert_eq!(
            base_count_after, 10,
            "base view must not see uncommitted insert"
        );
    }

    #[test]
    fn no_proof_path_not_found_returns_error() {
        // Querying a path whose parent layer doesn't exist must surface
        // the same path-not-found error other reads produce — exercises
        // the open_transactional_merk_at_path error arm.
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"does-not-exist".to_vec()],
            QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
        );
        let result = db
            .grove_db
            .query_aggregate_count(&path_query, None, v)
            .unwrap();
        assert!(
            result.is_err(),
            "querying a non-existent path must fail, got Ok({:?})",
            result.ok()
        );
    }

    #[test]
    fn no_proof_empty_provable_count_tree_returns_zero() {
        // An empty provable-count tree should walk in O(1) and return 0
        // — no proof generation, no merk traversal beyond the root open.
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"empty",
            Element::empty_provable_count_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert empty provable count tree");
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"empty".to_vec()],
            QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
        );
        let count = db
            .grove_db
            .query_aggregate_count(&path_query, None, v)
            .unwrap()
            .expect("query_aggregate_count on empty tree should succeed");
        assert_eq!(count, 0, "empty tree must return 0");
    }

    // ---------- No-proof per-key entry point ----------
    //
    // `query_aggregate_count_per_key` is the no-proof counterpart of
    // `verify_aggregate_count_query_per_key`: same surface shape
    // (`Vec<(Vec<u8>, u64)>`), accepts both leaf and carrier path
    // queries, but skips proof generation and verification entirely.

    #[test]
    fn no_proof_per_key_leaf_matches_single_count() {
        // Leaf-shape path query → returns a one-entry vec with an
        // empty key and the same count `query_aggregate_count`
        // returns (the per-key entry's leaf-symmetry contract).
        let v = GroveVersion::latest();
        let (db, _) = setup_15_key_provable_count_tree(v);
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
        );

        let single = db
            .grove_db
            .query_aggregate_count(&path_query, None, v)
            .unwrap()
            .expect("legacy single-u64 entry should succeed");
        let per_key = db
            .grove_db
            .query_aggregate_count_per_key(&path_query, None, v)
            .unwrap()
            .expect("per-key entry should succeed");

        assert_eq!(single, 10);
        assert_eq!(per_key.len(), 1);
        assert_eq!(per_key[0].0, Vec::<u8>::new());
        assert_eq!(per_key[0].1, single);
    }

    #[test]
    fn no_proof_per_key_carrier_returns_per_outer_count() {
        // Carrier shape → one (brand, count) entry per matched outer
        // key, mirroring `verify_aggregate_count_query_per_key`'s
        // contract.
        let v = GroveVersion::latest();
        let (db, _root) = setup_brand_color_carrier_tree(v, &[b"brand_000", b"brand_001"], 1_000);
        let path_query = carrier_count_path_query(
            &[b"brand_000", b"brand_001"],
            QueryItem::RangeAfter(b"color_00499".to_vec()..),
        );
        let results = db
            .grove_db
            .query_aggregate_count_per_key(&path_query, None, v)
            .unwrap()
            .expect("no-proof carrier query should succeed");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, b"brand_000".to_vec());
        assert_eq!(results[1].0, b"brand_001".to_vec());
        assert_eq!(results[0].1, 500);
        assert_eq!(results[1].1, 500);
    }

    #[test]
    fn no_proof_per_key_skips_absent_outer_keys() {
        // Absent outer keys contribute no entry — same as the proof
        // path's behavior.
        let v = GroveVersion::latest();
        let (db, _root) = setup_brand_color_carrier_tree(v, &[b"brand_000"], 100);
        let path_query = carrier_count_path_query(
            &[b"brand_000", b"brand_missing"],
            QueryItem::RangeAfter(b"color_00049".to_vec()..),
        );
        let results = db
            .grove_db
            .query_aggregate_count_per_key(&path_query, None, v)
            .unwrap()
            .expect("no-proof carrier query should succeed");
        assert_eq!(results.len(), 1, "absent key contributes no entry");
        assert_eq!(results[0].0, b"brand_000".to_vec());
        assert_eq!(results[0].1, 50);
    }

    #[test]
    fn no_proof_per_key_empty_leaf_returns_zero() {
        // Outer key exists, subquery_path resolves cleanly, but the
        // leaf count tree is empty. Match the proof path: emit
        // `(key, 0)` rather than skipping or erroring.
        use grovedb_query::Query;
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"byBrand",
            Element::empty_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert byBrand");
        db.insert(
            [TEST_LEAF, b"byBrand"].as_ref(),
            b"brand_000",
            Element::empty_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert brand");
        db.insert(
            [TEST_LEAF, b"byBrand", b"brand_000"].as_ref(),
            b"color",
            Element::empty_provable_count_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert empty color");

        let mut carrier = Query::new();
        carrier.insert_key(b"brand_000".to_vec());
        carrier.set_subquery_path(vec![b"color".to_vec()]);
        carrier.set_subquery(Query::new_aggregate_count_on_range(QueryItem::Range(
            b"a".to_vec()..b"z".to_vec(),
        )));
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"byBrand".to_vec()],
            SizedQuery::new(carrier, None, None),
        );
        let results = db
            .grove_db
            .query_aggregate_count_per_key(&path_query, None, v)
            .unwrap()
            .expect("no-proof carrier with empty leaf should succeed");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, b"brand_000".to_vec());
        assert_eq!(results[0].1, 0);
    }

    #[test]
    fn no_proof_per_key_matches_proof_path_per_key() {
        // Cross-check: for a non-trivial carrier query, the no-proof
        // result must agree element-for-element with the proof-based
        // `verify_aggregate_count_query_per_key`.
        let v = GroveVersion::latest();
        let (db, _root) =
            setup_brand_color_carrier_tree(v, &[b"brand_000", b"brand_001", b"brand_002"], 100);
        let path_query = carrier_count_path_query(
            &[b"brand_000", b"brand_001", b"brand_002"],
            QueryItem::RangeAfter(b"color_00049".to_vec()..),
        );
        let no_proof = db
            .grove_db
            .query_aggregate_count_per_key(&path_query, None, v)
            .unwrap()
            .expect("no-proof should succeed");
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");
        let (_root, proved) = GroveDb::verify_aggregate_count_query_per_key(&proof, &path_query, v)
            .expect("verify should succeed");
        assert_eq!(no_proof, proved);
    }

    #[test]
    fn no_proof_per_key_rejects_non_aggregate_count_query() {
        // Same validation gate as the proof per-key entry: non-ACOR
        // path queries are rejected up front with `InvalidQuery`.
        let v = GroveVersion::latest();
        let path_query = PathQuery::new_single_query_item(
            vec![TEST_LEAF.to_vec()],
            QueryItem::Key(b"k".to_vec()),
        );
        let db = make_test_grovedb(v);
        let err = db
            .grove_db
            .query_aggregate_count_per_key(&path_query, None, v)
            .unwrap()
            .expect_err("non-aggregate-count path query must be rejected");
        assert!(matches!(err, crate::Error::InvalidQuery(_)));
    }

    // ---------- Carrier aggregate-count end-to-end tests ----------
    //
    // A "carrier" aggregate-count query is an outer fan-out — the outer query items
    // are `Key`/`Range*` and the `default_subquery_branch.subquery`
    // resolves (after walking the optional `subquery_path`) to a leaf
    // aggregate-count. The verifier returns one `(outer_key, u64)` pair per matched
    // outer key. These tests exercise the full prove → encode → decode →
    // verify pipeline.

    /// Build a 3-deep tree shaped like the Dash Platform GROUP BY use
    /// case: `TEST_LEAF / "byBrand" / <brand_n> / "color" /
    /// <ProvableCountTree of color_xxx items>`.
    ///
    /// Each brand subtree has a `color` child that is a
    /// `ProvableCountTree` populated with `colors_per_brand` keys of the
    /// form `color_<i:05>`.
    fn setup_brand_color_carrier_tree(
        grove_version: &GroveVersion,
        brands: &[&[u8]],
        colors_per_brand: u32,
    ) -> (crate::tests::TempGroveDb, [u8; 32]) {
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"byBrand",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert byBrand");
        for brand in brands {
            db.insert(
                [TEST_LEAF, b"byBrand"].as_ref(),
                brand,
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert brand");
            db.insert(
                [TEST_LEAF, b"byBrand", brand].as_ref(),
                b"color",
                Element::empty_provable_count_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert color subtree");
            for i in 0..colors_per_brand {
                let key = format!("color_{:05}", i);
                db.insert(
                    [TEST_LEAF, b"byBrand", brand, b"color"].as_ref(),
                    key.as_bytes(),
                    Element::new_item(key.as_bytes().to_vec()),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert color leaf");
            }
        }
        let root = db
            .grove_db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root_hash");
        (db, root)
    }

    /// Build a carrier aggregate-count `PathQuery` rooted at
    /// `[TEST_LEAF, "byBrand"]`, fanning out across `outer_keys` and
    /// counting elements in each brand's `color` subtree matching the
    /// inner range.
    fn carrier_count_path_query(outer_keys: &[&[u8]], inner_range: QueryItem) -> PathQuery {
        use grovedb_query::Query;

        let mut carrier = Query::new();
        for k in outer_keys {
            // Use `insert_key` (not `items.push`) so items end up in
            // sorted-ascending order — the merk multi-key walker
            // expects that invariant.
            carrier.insert_key(k.to_vec());
        }
        carrier.set_subquery_path(vec![b"color".to_vec()]);
        carrier.set_subquery(Query::new_aggregate_count_on_range(inner_range));

        PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"byBrand".to_vec()],
            SizedQuery::new(carrier, None, None),
        )
    }

    #[test]
    fn carrier_two_outer_keys_succeeds() {
        // Carrier with two outer brand keys, range on the color subtree.
        // Expected: two (key, count) pairs in query-direction order with
        // the correct per-brand aggregate. The carrier defaults to
        // `left_to_right=true`, so output is ascending lex.
        let v = GroveVersion::latest();
        let (db, expected_root) =
            setup_brand_color_carrier_tree(v, &[b"brand_000", b"brand_001"], 1_000);
        // Pick a range that drops the lower 500 elements (`color_00000`
        // through `color_00499`).
        let path_query = carrier_count_path_query(
            &[b"brand_000", b"brand_001"],
            QueryItem::RangeAfter(b"color_00499".to_vec()..),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query (carrier aggregate-count) should succeed");
        let (got_root, results) =
            GroveDb::verify_aggregate_count_query_per_key(&proof, &path_query, v)
                .expect("verify carrier aggregate-count should succeed");
        assert_eq!(got_root, expected_root, "root must match GroveDB root");
        assert_eq!(results.len(), 2, "expected one result per outer key");
        assert_eq!(results[0].0, b"brand_000".to_vec());
        assert_eq!(results[1].0, b"brand_001".to_vec());
        // Each brand has 1 000 colors; range_after `color_00499` leaves
        // the upper 500 (`color_00500` .. `color_00999`).
        assert_eq!(results[0].1, 500);
        assert_eq!(results[1].1, 500);
    }

    #[test]
    fn carrier_with_unknown_outer_key_returns_present_keys_only() {
        // Spec acceptance criterion 2: an outer-key match that doesn't
        // exist contributes no entry to the result vector (it's an
        // absence, not an error). The prover doesn't emit a lower layer
        // for keys that don't exist in the carrier subtree, so the
        // verifier sees only the matched keys.
        let v = GroveVersion::latest();
        let (db, expected_root) = setup_brand_color_carrier_tree(v, &[b"brand_000"], 1_000);
        // Ask for two brands — one present, one absent.
        let path_query = carrier_count_path_query(
            &[b"brand_000", b"brand_999_missing"],
            QueryItem::RangeAfter(b"color_00499".to_vec()..),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");
        let (got_root, results) =
            GroveDb::verify_aggregate_count_query_per_key(&proof, &path_query, v)
                .expect("verify should succeed");
        assert_eq!(got_root, expected_root);
        // Only the present brand contributes a result row.
        assert_eq!(
            results.len(),
            1,
            "absent outer keys must not contribute an entry"
        );
        assert_eq!(results[0].0, b"brand_000".to_vec());
        assert_eq!(results[0].1, 500);
    }

    #[test]
    fn rejects_nested_carrier_aggregate_over_value_range_count() {
        // Out of scope: a "Range × Range × AggregateCountOnRange"
        // shape — an outer carrier whose subquery is *itself* another
        // carrier. This is the `IN × IN`-on-prefix case the spec
        // explicitly defers. The carrier validator delegates to the
        // leaf validator for the subquery, which rejects because the
        // inner carrier has its own outer items (not a single
        // `AggregateCountOnRange`). Both the static validator and the
        // prover's entry-point gate must refuse.
        use grovedb_query::Query;

        // inner_carrier: Range outer + leaf aggregate-count subquery.
        let mut inner_carrier = Query::new();
        inner_carrier
            .items
            .push(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        inner_carrier.set_subquery_path(vec![b"leaf".to_vec()]);
        inner_carrier.set_subquery(Query::new_aggregate_count_on_range(QueryItem::Range(
            b"a".to_vec()..b"z".to_vec(),
        )));

        // outer_carrier: Range outer + inner_carrier as subquery.
        let mut outer_carrier = Query::new();
        outer_carrier
            .items
            .push(QueryItem::Range(b"A".to_vec()..b"Z".to_vec()));
        outer_carrier.set_subquery_path(vec![b"middle".to_vec()]);
        outer_carrier.set_subquery(inner_carrier);

        let pq = PathQuery::new(
            vec![TEST_LEAF.to_vec()],
            SizedQuery::new(outer_carrier, None, None),
        );
        let v = GroveVersion::latest();

        // Static validator rejects.
        assert!(
            pq.validate_aggregate_count_on_range().is_err(),
            "nested carrier (Range x Range x ACOR) must fail validation"
        );

        // Prover entry-point gate also rejects.
        let prove_result = make_test_grovedb(v).grove_db.prove_query(&pq, None, v);
        match prove_result.value() {
            Err(crate::Error::InvalidQuery(_)) => {}
            other => panic!("expected InvalidQuery, got {:?}", other),
        }
    }

    #[test]
    fn rejects_aggregate_count_at_both_levels() {
        // Try to build a query where the carrier ITSELF has an aggregate-count item
        // AND its subquery is also an aggregate-count. The validator must reject up
        // front at prove time.
        use grovedb_query::Query;

        let mut q =
            Query::new_aggregate_count_on_range(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        q.set_subquery(Query::new_aggregate_count_on_range(QueryItem::Range(
            b"a".to_vec()..b"z".to_vec(),
        )));
        let pq = PathQuery::new(vec![TEST_LEAF.to_vec()], SizedQuery::new(q, None, None));
        let v = GroveVersion::latest();
        // Validation catches it.
        assert!(
            pq.validate_aggregate_count_on_range().is_err(),
            "aggregate-count + subquery aggregate-count must fail validation"
        );
        // The prove_query entry-point gate must also reject it.
        let prove_result = make_test_grovedb(v).grove_db.prove_query(&pq, None, v);
        match prove_result.value() {
            Err(crate::Error::InvalidQuery(_)) => {}
            other => panic!("expected InvalidQuery, got {:?}", other),
        }
    }

    #[test]
    fn leaf_unchanged_under_per_key_verifier() {
        // The leaf shape — a single-`AggregateCountOnRange` query —
        // produces the same proof bytes whether the caller verifies via
        // `verify_aggregate_count_query` or the per-key entry point.
        // Verifying it via the per-key entry point returns a one-entry
        // Vec with an empty key and the same count
        // `verify_aggregate_count_query` returns. This is the
        // leaf-symmetry contract.
        let v = GroveVersion::latest();
        let (db, expected_root) = setup_15_key_provable_count_tree(v);
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");
        // Existing single-u64 entry point still works.
        let (root_one, count_one) = GroveDb::verify_aggregate_count_query(&proof, &path_query, v)
            .expect("legacy leaf verifier must still accept legacy leaf proof");
        // New per-key entry point also accepts leaf and returns a
        // one-entry Vec with an empty key.
        let (root_many, results) =
            GroveDb::verify_aggregate_count_query_per_key(&proof, &path_query, v)
                .expect("per-key verifier must accept leaf proofs");
        assert_eq!(root_one, expected_root);
        assert_eq!(root_one, root_many);
        assert_eq!(count_one, 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, Vec::<u8>::new());
        assert_eq!(results[0].1, count_one);
    }

    #[test]
    fn carrier_with_range_outer_succeeds() {
        // The carrier supports a Range outer item (the per-spec
        // "decide-or-defer" case). With an outer `RangeAfter`, the
        // matched outer keys come back in lex-asc order and each
        // contributes its own count.
        use grovedb_query::Query;
        let v = GroveVersion::latest();
        let (db, expected_root) =
            setup_brand_color_carrier_tree(v, &[b"brand_000", b"brand_001", b"brand_002"], 100);

        let mut carrier = Query::new();
        // Take everything strictly after brand_000 → brand_001, brand_002.
        carrier
            .items
            .push(QueryItem::RangeAfter(b"brand_000".to_vec()..));
        carrier.set_subquery_path(vec![b"color".to_vec()]);
        carrier.set_subquery(Query::new_aggregate_count_on_range(QueryItem::RangeAfter(
            b"color_00049".to_vec()..,
        )));
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"byBrand".to_vec()],
            SizedQuery::new(carrier, None, None),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query (carrier with Range outer) should succeed");
        let (got_root, results) =
            GroveDb::verify_aggregate_count_query_per_key(&proof, &path_query, v)
                .expect("verify carrier with Range outer should succeed");
        assert_eq!(got_root, expected_root);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, b"brand_001".to_vec());
        assert_eq!(results[1].0, b"brand_002".to_vec());
        for (_, count) in results {
            // 100 colors per brand; > color_00049 leaves 50.
            assert_eq!(count, 50);
        }
    }

    #[test]
    fn carrier_right_to_left_returns_descending_order() {
        // Flip the carrier's `left_to_right` flag — output must come
        // back in descending lex order, mirroring the merk walker's
        // reversed emission.
        use grovedb_query::Query;
        let v = GroveVersion::latest();
        let (db, expected_root) =
            setup_brand_color_carrier_tree(v, &[b"brand_000", b"brand_001", b"brand_002"], 100);
        let mut carrier = Query::new_with_direction(false);
        carrier.insert_key(b"brand_000".to_vec());
        carrier.insert_key(b"brand_001".to_vec());
        carrier.insert_key(b"brand_002".to_vec());
        carrier.set_subquery_path(vec![b"color".to_vec()]);
        carrier.set_subquery(Query::new_aggregate_count_on_range(QueryItem::RangeAfter(
            b"color_00049".to_vec()..,
        )));
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"byBrand".to_vec()],
            SizedQuery::new(carrier, None, None),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query (carrier aggregate-count, right-to-left) should succeed");
        let (got_root, results) =
            GroveDb::verify_aggregate_count_query_per_key(&proof, &path_query, v)
                .expect("verify carrier aggregate-count (right-to-left) should succeed");
        assert_eq!(got_root, expected_root);
        assert_eq!(results.len(), 3, "expected 3 outer-key matches");
        // Descending lex: brand_002, brand_001, brand_000.
        assert_eq!(results[0].0, b"brand_002".to_vec());
        assert_eq!(results[1].0, b"brand_001".to_vec());
        assert_eq!(results[2].0, b"brand_000".to_vec());
        for (_, count) in results {
            assert_eq!(count, 50);
        }
    }

    #[test]
    fn per_key_rejects_non_aggregate_count_path_query() {
        // The per-key entry point rejects path queries that aren't aggregate-count
        // queries at all — neither leaf nor carrier — before decoding
        // proof bytes.
        let v = GroveVersion::latest();
        let bad_query = PathQuery::new_single_query_item(
            vec![TEST_LEAF.to_vec()],
            QueryItem::Key(b"k".to_vec()),
        );
        let dummy_proof = vec![0u8; 16];
        let err = GroveDb::verify_aggregate_count_query_per_key(&dummy_proof, &bad_query, v)
            .expect_err("non-aggregate-count path_query must be rejected up front");
        match err {
            crate::Error::InvalidQuery(_) => {}
            other => panic!("expected InvalidQuery, got {:?}", other),
        }
    }

    #[test]
    fn carrier_count_forgery_is_caught() {
        // Same spirit as `count_forgery_is_caught_at_grovedb_level` but
        // against a carrier proof: pick the first leaf merk
        // `HashWithCount` op in any of the per-outer-key sub-proofs and
        // bump its count. The verifier must reject.
        use bincode::config;
        use grovedb_merk::proofs::{encoding::encode_into, Decoder, Node, Op};

        use crate::operations::proof::{
            GroveDBProof, GroveDBProofV0, GroveDBProofV1, LayerProof, MerkOnlyLayerProof,
            ProofBytes,
        };

        let v = GroveVersion::latest();
        let (db, _root) = setup_brand_color_carrier_tree(v, &[b"brand_000", b"brand_001"], 100);
        let path_query = carrier_count_path_query(
            &[b"brand_000", b"brand_001"],
            QueryItem::RangeAfter(b"color_00049".to_vec()..),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");

        let cfg = config::standard()
            .with_big_endian()
            .with_limit::<{ 256 * 1024 * 1024 }>();
        let (mut decoded, _): (GroveDBProof, _) =
            bincode::decode_from_slice(&proof, cfg).expect("decode envelope");

        // Walk to the first leaf merk proof bytes via depth-first
        // descent: we expect the path `TEST_LEAF -> byBrand -> brand_000
        // -> color`. The "leaf" is the deepest layer (a leaf has no
        // further lower_layers; in our test setup that's the count proof
        // at the color subtree).
        fn first_leaf_v0(mut layer: &mut MerkOnlyLayerProof) -> &mut Vec<u8> {
            while let Some((_, child)) = layer.lower_layers.iter_mut().next() {
                layer = child;
            }
            &mut layer.merk_proof
        }
        fn first_leaf_v1(mut layer: &mut LayerProof) -> &mut Vec<u8> {
            while let Some((_, child)) = layer.lower_layers.iter_mut().next() {
                layer = child;
            }
            match &mut layer.merk_proof {
                ProofBytes::Merk(b) => b,
                _ => panic!("expected Merk leaf bytes"),
            }
        }
        let leaf_bytes: &mut Vec<u8> = match &mut decoded {
            GroveDBProof::V0(GroveDBProofV0 { root_layer, .. }) => first_leaf_v0(root_layer),
            GroveDBProof::V1(GroveDBProofV1 { root_layer }) => first_leaf_v1(root_layer),
        };
        let mut ops: Vec<Op> = Decoder::new(leaf_bytes)
            .map(|r| r.expect("decode op"))
            .collect();
        let mut tampered = false;
        for op in ops.iter_mut() {
            match op {
                Op::Push(Node::HashWithCount(_, _, _, count))
                | Op::PushInverted(Node::HashWithCount(_, _, _, count)) => {
                    *count = count.wrapping_add(1);
                    tampered = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(tampered, "expected a HashWithCount in the leaf proof");
        let mut new_leaf = Vec::new();
        encode_into(ops.iter(), &mut new_leaf);
        *leaf_bytes = new_leaf;
        let new_proof = bincode::encode_to_vec(
            decoded,
            config::standard().with_big_endian().with_no_limit(),
        )
        .expect("re-encode");

        let result = GroveDb::verify_aggregate_count_query_per_key(&new_proof, &path_query, v);
        assert!(
            result.is_err(),
            "tampered carrier count must be rejected, got {:?}",
            result.map(|(_, c)| c)
        );
    }

    #[test]
    fn carrier_rejects_v0_envelope() {
        // V0 proof envelopes predate aggregate-count and cannot legitimately carry
        // an aggregate-count proof — neither leaf nor carrier. The
        // prover-side entry-point gate refuses to emit V0+aggregate-count.
        let v2 = &GROVE_V2;
        let (db, _root) = setup_brand_color_carrier_tree(v2, &[b"brand_000"], 100);
        let path_query = carrier_count_path_query(
            &[b"brand_000"],
            QueryItem::RangeAfter(b"color_00049".to_vec()..),
        );
        match db.grove_db.prove_query(&path_query, None, v2).unwrap() {
            Err(crate::Error::NotSupported(msg)) => assert!(
                msg.contains("V1 proof envelopes"),
                "unexpected message: {msg}"
            ),
            other => panic!(
                "expected NotSupported for V0+carrier aggregate-count, got {:?}",
                other.map(|b| b.len())
            ),
        }
    }

    #[test]
    fn carrier_sql_style_fixed_prefix_range_then_count_succeeds() {
        // Demonstrates the SQL-style 3-column aggregate query
        //
        //   SELECT COUNT(*) FROM t WHERE a = 1 AND b > 4 AND c > 4
        //
        // against an `(a, b, c)`-indexed grove laid out as
        //
        //   TEST_LEAF / byA / <a_val> / byB / <b_val> / byC /
        //       <ProvableCountTree of c_val items>
        //
        // The mapping is:
        //   - `A = 1` is a fixed prefix → lives in `path_query.path`
        //     (the verifier walks it via single-key descents).
        //   - `B > 4` is the variable outer dimension → carrier's
        //     `RangeAfter("b_4")` item.
        //   - per matched `B`, walk `byC` → carrier's `subquery_path`.
        //   - `COUNT(C > 4)` is the leaf aggregate-count subquery.
        //
        // Expected: one `(b_val, count)` entry per matched `b > 4`,
        // each carrying the count of `c > 4` under that `(a=1, b)` cell.
        use grovedb_query::Query;
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);

        // Build the tree: TEST_LEAF/byA/1/byB/<b>/byC/<c>.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"byA",
            Element::empty_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert byA");
        // Insert two `A` values so we can confirm the path prefix
        // actually scopes the count (queries with `A = 1` must not
        // see anything under `A = 2`).
        for a_val in [b"1".as_ref(), b"2".as_ref()] {
            db.insert(
                [TEST_LEAF, b"byA"].as_ref(),
                a_val,
                Element::empty_tree(),
                None,
                None,
                v,
            )
            .unwrap()
            .expect("insert a_val");
            db.insert(
                [TEST_LEAF, b"byA", a_val].as_ref(),
                b"byB",
                Element::empty_tree(),
                None,
                None,
                v,
            )
            .unwrap()
            .expect("insert byB");
            for b_val in [b"b_3".as_ref(), b"b_5".as_ref(), b"b_7".as_ref()] {
                db.insert(
                    [TEST_LEAF, b"byA", a_val, b"byB"].as_ref(),
                    b_val,
                    Element::empty_tree(),
                    None,
                    None,
                    v,
                )
                .unwrap()
                .expect("insert b_val");
                db.insert(
                    [TEST_LEAF, b"byA", a_val, b"byB", b_val].as_ref(),
                    b"byC",
                    Element::empty_provable_count_tree(),
                    None,
                    None,
                    v,
                )
                .unwrap()
                .expect("insert byC");
                for i in 0..10u8 {
                    let c_key = format!("c_{i}");
                    db.insert(
                        [TEST_LEAF, b"byA", a_val, b"byB", b_val, b"byC"].as_ref(),
                        c_key.as_bytes(),
                        Element::new_item(c_key.as_bytes().to_vec()),
                        None,
                        None,
                        v,
                    )
                    .unwrap()
                    .expect("insert c");
                }
            }
        }
        let expected_root = db.grove_db.root_hash(None, v).unwrap().expect("root_hash");

        // Carrier: `B > "b_4"` outer, walk `byC`, count `C > "c_4"`.
        let mut carrier = Query::new();
        carrier.items.push(QueryItem::RangeAfter(b"b_4".to_vec()..));
        carrier.set_subquery_path(vec![b"byC".to_vec()]);
        carrier.set_subquery(Query::new_aggregate_count_on_range(QueryItem::RangeAfter(
            b"c_4".to_vec()..,
        )));

        // PathQuery: fix `A = 1` via the path prefix.
        let path_query = PathQuery::new(
            vec![
                TEST_LEAF.to_vec(),
                b"byA".to_vec(),
                b"1".to_vec(),
                b"byB".to_vec(),
            ],
            SizedQuery::new(carrier, None, None),
        );

        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove (A=1, B>4, COUNT C>4) should succeed");
        let (got_root, results) =
            GroveDb::verify_aggregate_count_query_per_key(&proof, &path_query, v)
                .expect("verify should succeed");
        assert_eq!(got_root, expected_root);
        // B > "b_4" matches `b_5` and `b_7` (not `b_3`). For each
        // matched B, count C > "c_4" → c_5..=c_9 → 5 elements.
        assert_eq!(results.len(), 2, "expected b_5 and b_7");
        assert_eq!(results[0].0, b"b_5".to_vec());
        assert_eq!(results[1].0, b"b_7".to_vec());
        assert_eq!(results[0].1, 5);
        assert_eq!(results[1].1, 5);
    }

    #[test]
    fn carrier_returns_zero_count_for_empty_leaf_subtree() {
        // An outer-key match exists, the subquery_path resolves
        // cleanly, but the **leaf** `ProvableCountTree` is empty
        // (root_key = None). The verifier must still get a clean
        // `(brand, 0)` entry — the leaf count proof for an empty
        // merk is the empty op stream, which the merk-level verifier
        // reads as `(NULL_HASH, 0)`, and the chain check
        // `combine_hash(H(empty_tree_value), NULL_HASH) ==
        //  parent_value_hash` holds because the parent committed the
        // tree element with the same NULL_HASH child root.
        use grovedb_query::Query;
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"byBrand",
            Element::empty_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert byBrand");
        db.insert(
            [TEST_LEAF, b"byBrand"].as_ref(),
            b"brand_000",
            Element::empty_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert brand");
        // Insert the leaf `color` count tree but leave it empty.
        db.insert(
            [TEST_LEAF, b"byBrand", b"brand_000"].as_ref(),
            b"color",
            Element::empty_provable_count_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert empty color count tree");
        let expected_root = db.grove_db.root_hash(None, v).unwrap().expect("root_hash");

        let mut carrier = Query::new();
        carrier.insert_key(b"brand_000".to_vec());
        carrier.set_subquery_path(vec![b"color".to_vec()]);
        carrier.set_subquery(Query::new_aggregate_count_on_range(QueryItem::Range(
            b"a".to_vec()..b"z".to_vec(),
        )));
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"byBrand".to_vec()],
            SizedQuery::new(carrier, None, None),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed even when leaf count tree is empty");
        let (got_root, results) =
            GroveDb::verify_aggregate_count_query_per_key(&proof, &path_query, v)
                .expect("verify should succeed with (brand_000, 0)");
        assert_eq!(got_root, expected_root);
        assert_eq!(results.len(), 1, "expected one entry for brand_000");
        assert_eq!(results[0].0, b"brand_000".to_vec());
        assert_eq!(
            results[0].1, 0,
            "empty leaf count tree must yield count = 0"
        );
    }

    #[test]
    fn carrier_with_long_subquery_path_succeeds() {
        // Exercises a non-trivial `subquery_path` (length > 1) in the
        // carrier shape: TEST_LEAF / "outer" / <brand> / "level1" /
        // "level2" / <ProvableCountTree>. The verifier must walk both
        // intermediate single-key layers between each outer-key match
        // and the leaf merk.
        use grovedb_query::Query;
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::empty_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert outer");
        for brand in [b"a".as_ref(), b"b".as_ref()] {
            db.insert(
                [TEST_LEAF, b"outer"].as_ref(),
                brand,
                Element::empty_tree(),
                None,
                None,
                v,
            )
            .unwrap()
            .expect("insert brand");
            db.insert(
                [TEST_LEAF, b"outer", brand].as_ref(),
                b"level1",
                Element::empty_tree(),
                None,
                None,
                v,
            )
            .unwrap()
            .expect("insert level1");
            db.insert(
                [TEST_LEAF, b"outer", brand, b"level1"].as_ref(),
                b"level2",
                Element::empty_provable_count_tree(),
                None,
                None,
                v,
            )
            .unwrap()
            .expect("insert level2");
            for c in b'a'..=b'e' {
                db.insert(
                    [TEST_LEAF, b"outer", brand, b"level1", b"level2"].as_ref(),
                    &[c],
                    Element::new_item(vec![c]),
                    None,
                    None,
                    v,
                )
                .unwrap()
                .expect("insert leaf");
            }
        }
        let expected_root = db.grove_db.root_hash(None, v).unwrap().expect("root_hash");

        // Carrier path query: walks "level1" → "level2" between each
        // outer-brand match and the leaf count proof.
        let mut carrier = Query::new();
        carrier.insert_key(b"a".to_vec());
        carrier.insert_key(b"b".to_vec());
        carrier.set_subquery_path(vec![b"level1".to_vec(), b"level2".to_vec()]);
        carrier.set_subquery(Query::new_aggregate_count_on_range(
            QueryItem::RangeInclusive(b"b".to_vec()..=b"d".to_vec()),
        ));
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"outer".to_vec()],
            SizedQuery::new(carrier, None, None),
        );

        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query (carrier with long subquery_path) should succeed");
        let (got_root, results) =
            GroveDb::verify_aggregate_count_query_per_key(&proof, &path_query, v)
                .expect("verify carrier (long subquery_path) should succeed");
        assert_eq!(got_root, expected_root);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, b"a".to_vec());
        assert_eq!(results[1].0, b"b".to_vec());
        assert_eq!(results[0].1, 3, "{{b, c, d}} expected for brand a");
        assert_eq!(results[1].1, 3, "{{b, c, d}} expected for brand b");
    }

    #[test]
    fn carrier_corrupted_outer_layer_byte_is_rejected() {
        // Flip a byte deep inside the carrier-layer merk proof bytes
        // (which encode the outer-Keys multi-key proof). Either the
        // merk-level execute_proof rejects the bytes, or the chain
        // check downstream rejects the resulting hash mismatch.
        let v = GroveVersion::latest();
        let (db, _root) = setup_brand_color_carrier_tree(v, &[b"brand_000", b"brand_001"], 100);
        let path_query = carrier_count_path_query(
            &[b"brand_000", b"brand_001"],
            QueryItem::RangeAfter(b"color_00049".to_vec()..),
        );
        let mut proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");
        // Flip a byte ~3/4 through the proof — far enough into the
        // envelope to land inside the carrier-layer merk_proof bytes.
        let target = (proof.len() * 3) / 4;
        proof[target] ^= 0x55;
        let result = GroveDb::verify_aggregate_count_query_per_key(&proof, &path_query, v);
        assert!(
            result.is_err(),
            "tampered carrier-layer byte must be rejected, got {:?}",
            result.map(|(_, c)| c)
        );
    }

    #[test]
    fn carrier_undecodable_proof_is_rejected() {
        // Send garbage bytes — the bincode decoder rejects the
        // envelope up front with `Error::CorruptedData`.
        let v = GroveVersion::latest();
        let path_query = carrier_count_path_query(
            &[b"brand_000"],
            QueryItem::RangeAfter(b"color_00049".to_vec()..),
        );
        let garbage = vec![0xffu8; 32];
        let err = GroveDb::verify_aggregate_count_query_per_key(&garbage, &path_query, v)
            .expect_err("undecodable proof must be rejected");
        match err {
            crate::Error::CorruptedData(_) => {}
            other => panic!("expected CorruptedData, got {:?}", other),
        }
    }

    #[test]
    fn aggregate_count_proof_with_trailing_bytes_is_rejected() {
        // Decoding is canonical — a valid proof with any trailing
        // bytes appended must be rejected, even though the
        // cryptographic chain check would still bind the same
        // `(RootHash, count)` result. Otherwise the same logical
        // proof would have many distinct byte encodings, which breaks
        // proof-equality / caching assumptions.
        let v = GroveVersion::latest();
        let (db, _root) = setup_15_key_provable_count_tree(v);
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
        );
        let mut proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");
        // Sanity: the untouched proof verifies.
        GroveDb::verify_aggregate_count_query(&proof, &path_query, v)
            .expect("clean proof should verify");
        // Now append a single trailing byte and expect rejection from
        // both entry points.
        proof.push(0u8);
        let leaf_err = GroveDb::verify_aggregate_count_query(&proof, &path_query, v)
            .expect_err("leaf entry: trailing-byte proof must be rejected");
        match leaf_err {
            crate::Error::CorruptedData(msg) => {
                assert!(msg.contains("trailing bytes"), "unexpected message: {msg}")
            }
            other => panic!("expected CorruptedData, got {:?}", other),
        }
        let per_key_err = GroveDb::verify_aggregate_count_query_per_key(&proof, &path_query, v)
            .expect_err("per-key entry: trailing-byte proof must be rejected");
        match per_key_err {
            crate::Error::CorruptedData(msg) => {
                assert!(msg.contains("trailing bytes"), "unexpected message: {msg}")
            }
            other => panic!("expected CorruptedData, got {:?}", other),
        }
    }

    #[test]
    fn carrier_legacy_verifier_rejects_carrier_query() {
        // The legacy single-`u64` `verify_aggregate_count_query` strictly
        // validates the leaf shape and rejects carrier queries — even
        // though the proof bytes themselves are well-formed. Callers
        // must use `verify_aggregate_count_query_per_key` for carriers.
        let v = GroveVersion::latest();
        let (db, _root) = setup_brand_color_carrier_tree(v, &[b"brand_000"], 50);
        let path_query = carrier_count_path_query(
            &[b"brand_000"],
            QueryItem::Range(b"color_00010".to_vec()..b"color_00020".to_vec()),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");
        let err = GroveDb::verify_aggregate_count_query(&proof, &path_query, v)
            .expect_err("legacy leaf verifier must reject carrier shape");
        match err {
            crate::Error::InvalidQuery(_) => {}
            other => panic!("expected InvalidQuery, got {:?}", other),
        }
    }

    #[test]
    fn carrier_missing_outer_lower_layer_is_rejected() {
        // Decode the carrier proof envelope, drop one of the
        // `lower_layers[outer_key]` entries, re-encode, and verify the
        // verifier rejects with "missing lower layer for outer key".
        // Exercises the `lower_layers.get(&outer_key).ok_or_else(...)`
        // branch in `verify_v1_carrier_layer`.
        use bincode::config;

        use crate::operations::proof::{GroveDBProof, GroveDBProofV1};

        let v = GroveVersion::latest();
        let (db, _root) = setup_brand_color_carrier_tree(v, &[b"brand_000", b"brand_001"], 100);
        let path_query = carrier_count_path_query(
            &[b"brand_000", b"brand_001"],
            QueryItem::RangeAfter(b"color_00049".to_vec()..),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");

        let cfg = config::standard()
            .with_big_endian()
            .with_limit::<{ 256 * 1024 * 1024 }>();
        let (mut decoded, _): (GroveDBProof, _) =
            bincode::decode_from_slice(&proof, cfg).expect("decode envelope");
        let GroveDBProof::V1(GroveDBProofV1 { root_layer }) = &mut decoded else {
            panic!("expected V1 envelope");
        };
        // Walk to the carrier layer: TEST_LEAF -> byBrand.
        let carrier_layer = root_layer
            .lower_layers
            .get_mut(&TEST_LEAF.to_vec())
            .expect("TEST_LEAF layer")
            .lower_layers
            .get_mut(&b"byBrand".to_vec())
            .expect("byBrand carrier layer");
        // Drop brand_001's lower_layer — its row will still be in the
        // multi-key proof but the descent will fail.
        let removed = carrier_layer.lower_layers.remove(&b"brand_001".to_vec());
        assert!(
            removed.is_some(),
            "test setup: expected brand_001 in carrier lower_layers"
        );
        let new_proof = bincode::encode_to_vec(
            decoded,
            config::standard().with_big_endian().with_no_limit(),
        )
        .expect("re-encode");

        let err = GroveDb::verify_aggregate_count_query_per_key(&new_proof, &path_query, v)
            .expect_err("missing outer lower_layer must be rejected");
        match err {
            crate::Error::InvalidProof(_, msg) => assert!(
                msg.contains("missing lower layer for outer key"),
                "unexpected message: {msg}"
            ),
            other => panic!("expected InvalidProof, got {:?}", other),
        }
    }

    #[test]
    fn carrier_missing_subquery_path_layer_is_rejected() {
        // Same idea as the previous test but one level deeper: drop the
        // `subquery_path` layer ("color") that sits between the outer
        // brand match and the leaf merk. Exercises the
        // `verify_v1_subquery_path` "missing subquery_path layer" branch.
        use bincode::config;

        use crate::operations::proof::{GroveDBProof, GroveDBProofV1};

        let v = GroveVersion::latest();
        let (db, _root) = setup_brand_color_carrier_tree(v, &[b"brand_000"], 100);
        let path_query = carrier_count_path_query(
            &[b"brand_000"],
            QueryItem::RangeAfter(b"color_00049".to_vec()..),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");

        let cfg = config::standard()
            .with_big_endian()
            .with_limit::<{ 256 * 1024 * 1024 }>();
        let (mut decoded, _): (GroveDBProof, _) =
            bincode::decode_from_slice(&proof, cfg).expect("decode envelope");
        let GroveDBProof::V1(GroveDBProofV1 { root_layer }) = &mut decoded else {
            panic!("expected V1 envelope");
        };
        // Walk to brand_000 layer and drop the "color" subquery_path
        // descent.
        let brand_layer = root_layer
            .lower_layers
            .get_mut(&TEST_LEAF.to_vec())
            .expect("TEST_LEAF layer")
            .lower_layers
            .get_mut(&b"byBrand".to_vec())
            .expect("byBrand layer")
            .lower_layers
            .get_mut(&b"brand_000".to_vec())
            .expect("brand_000 layer");
        let removed = brand_layer.lower_layers.remove(&b"color".to_vec());
        assert!(
            removed.is_some(),
            "test setup: expected color in brand_000 lower_layers"
        );
        let new_proof = bincode::encode_to_vec(
            decoded,
            config::standard().with_big_endian().with_no_limit(),
        )
        .expect("re-encode");

        let err = GroveDb::verify_aggregate_count_query_per_key(&new_proof, &path_query, v)
            .expect_err("missing subquery_path layer must be rejected");
        match err {
            crate::Error::InvalidProof(_, msg) => assert!(
                msg.contains("missing subquery_path layer"),
                "unexpected message: {msg}"
            ),
            other => panic!("expected InvalidProof, got {:?}", other),
        }
    }

    #[test]
    fn carrier_non_merk_proof_bytes_is_rejected() {
        // Replace the subquery_path layer's `ProofBytes::Merk(...)` with
        // a `ProofBytes::MMR(...)` variant. The verifier rejects the
        // mismatched proof-bytes flavor through `expect_merk_bytes`.
        use bincode::config;

        use crate::operations::proof::{GroveDBProof, GroveDBProofV1, ProofBytes};

        let v = GroveVersion::latest();
        let (db, _root) = setup_brand_color_carrier_tree(v, &[b"brand_000"], 100);
        let path_query = carrier_count_path_query(
            &[b"brand_000"],
            QueryItem::RangeAfter(b"color_00049".to_vec()..),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");
        let cfg = config::standard()
            .with_big_endian()
            .with_limit::<{ 256 * 1024 * 1024 }>();
        let (mut decoded, _): (GroveDBProof, _) =
            bincode::decode_from_slice(&proof, cfg).expect("decode envelope");
        let GroveDBProof::V1(GroveDBProofV1 { root_layer }) = &mut decoded else {
            panic!("expected V1 envelope");
        };
        // Swap the subquery_path "color" layer's proof bytes from Merk
        // to MMR — `verify_v1_subquery_path` will refuse via
        // `expect_merk_bytes`.
        let color_layer = root_layer
            .lower_layers
            .get_mut(&TEST_LEAF.to_vec())
            .expect("TEST_LEAF layer")
            .lower_layers
            .get_mut(&b"byBrand".to_vec())
            .expect("byBrand layer")
            .lower_layers
            .get_mut(&b"brand_000".to_vec())
            .expect("brand_000 layer")
            .lower_layers
            .get_mut(&b"color".to_vec())
            .expect("color layer");
        color_layer.merk_proof = ProofBytes::MMR(vec![]);
        let new_proof = bincode::encode_to_vec(
            decoded,
            config::standard().with_big_endian().with_no_limit(),
        )
        .expect("re-encode");

        let err = GroveDb::verify_aggregate_count_query_per_key(&new_proof, &path_query, v)
            .expect_err("non-Merk proof bytes must be rejected");
        match err {
            crate::Error::InvalidProof(_, msg) => assert!(
                msg.contains("unexpected non-merk layer bytes"),
                "unexpected message: {msg}"
            ),
            other => panic!("expected InvalidProof, got {:?}", other),
        }
    }

    #[test]
    fn carrier_aggregate_count_rejects_offset() {
        // Carriers still reject SizedQuery::offset: skipping the first
        // M outer matches changes which (outer_key, u64) pairs end up in
        // the proof, and the use case for that hasn't been designed
        // yet. The PathQuery-level validator surfaces this before any
        // proof bytes are decoded. (`limit` is now allowed — see
        // `carrier_*_with_limit_*` tests below.)
        use grovedb_query::Query;
        let v = GroveVersion::latest();
        let mut carrier = Query::new();
        carrier.insert_key(b"brand_000".to_vec());
        carrier.set_subquery_path(vec![b"color".to_vec()]);
        carrier.set_subquery(Query::new_aggregate_count_on_range(QueryItem::Range(
            b"color_00010".to_vec()..b"color_00020".to_vec(),
        )));
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"byBrand".to_vec()],
            SizedQuery::new(carrier, None, Some(2)),
        );
        let dummy_proof = vec![0u8; 8];
        let err = GroveDb::verify_aggregate_count_query_per_key(&dummy_proof, &path_query, v)
            .expect_err("carrier aggregate-count with offset must be rejected at entry");
        match err {
            crate::Error::InvalidQuery(msg) => {
                assert!(msg.contains("offset"), "unexpected message: {msg}");
                assert!(msg.contains("carrier"), "unexpected message: {msg}");
            }
            other => panic!("expected InvalidQuery, got {:?}", other),
        }
    }

    #[test]
    fn leaf_aggregate_count_still_rejects_limit() {
        // The leaf shape continues to reject SizedQuery::limit. A leaf
        // returns a single u64; pagination would silently change the
        // answer. This is the byte-identical behavior the leaf path had
        // before carrier limits were relaxed.
        let v = GroveVersion::latest();
        let mut path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
        );
        path_query.query.limit = Some(5);
        let dummy_proof = vec![0u8; 8];

        // Strict leaf verifier rejects.
        let err = GroveDb::verify_aggregate_count_query(&dummy_proof, &path_query, v)
            .expect_err("leaf aggregate-count with limit must be rejected at entry");
        match err {
            crate::Error::InvalidQuery(msg) => {
                assert!(msg.contains("leaf"), "unexpected message: {msg}");
                assert!(msg.contains("limit"), "unexpected message: {msg}");
            }
            other => panic!("expected InvalidQuery, got {:?}", other),
        }

        // The per-key entry point routes leaf queries through the leaf
        // validator too and rejects identically.
        let err = GroveDb::verify_aggregate_count_query_per_key(&dummy_proof, &path_query, v)
            .expect_err("per-key entry must also reject leaf-with-limit");
        match err {
            crate::Error::InvalidQuery(msg) => {
                assert!(msg.contains("leaf"), "unexpected message: {msg}");
                assert!(msg.contains("limit"), "unexpected message: {msg}");
            }
            other => panic!("expected InvalidQuery, got {:?}", other),
        }
    }

    #[test]
    fn carrier_keys_outer_with_limit_caps_results() {
        // Carrier ACOR with `Keys` outer items and `SizedQuery::limit`
        // set. The walk must stop after `limit` outer-key matches have
        // produced their leaf-ACOR u64 — each match is a complete
        // count, the inner range is not capped.
        use grovedb_query::Query;
        let v = GroveVersion::latest();
        let (db, expected_root) = setup_brand_color_carrier_tree(
            v,
            &[b"brand_000", b"brand_001", b"brand_002", b"brand_003"],
            100,
        );

        let mut carrier = Query::new();
        for k in [b"brand_000", b"brand_001", b"brand_002", b"brand_003"] {
            carrier.insert_key(k.to_vec());
        }
        carrier.set_subquery_path(vec![b"color".to_vec()]);
        carrier.set_subquery(Query::new_aggregate_count_on_range(QueryItem::RangeAfter(
            b"color_00049".to_vec()..,
        )));
        // Cap the outer walk at 2 matches.
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"byBrand".to_vec()],
            SizedQuery::new(carrier, Some(2), None),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query (carrier with Keys outer + limit) should succeed");
        let (got_root, results) =
            GroveDb::verify_aggregate_count_query_per_key(&proof, &path_query, v)
                .expect("verify carrier with Keys outer + limit should succeed");
        assert_eq!(got_root, expected_root, "root must match GroveDB root");
        assert_eq!(results.len(), 2, "expected exactly `limit` outer matches");
        // left_to_right defaults to true: first two brand keys ascending.
        assert_eq!(results[0].0, b"brand_000".to_vec());
        assert_eq!(results[1].0, b"brand_001".to_vec());
        for (_, count) in &results {
            // 100 colors per brand; > color_00049 leaves 50.
            assert_eq!(*count, 50);
        }
    }

    #[test]
    fn carrier_range_outer_with_limit_caps_results() {
        // Carrier ACOR with a `Range*` outer item and `SizedQuery::limit`
        // set — the "Q8 with outer Range" upstream use case. With 4
        // in-range brands and limit=2, the walk must return exactly 2
        // `(outer_key, u64)` pairs.
        use grovedb_query::Query;
        let v = GroveVersion::latest();
        let (db, expected_root) = setup_brand_color_carrier_tree(
            v,
            &[
                b"brand_000",
                b"brand_001",
                b"brand_002",
                b"brand_003",
                b"brand_004",
            ],
            100,
        );

        let mut carrier = Query::new();
        // After brand_000 → brand_001..=brand_004 (4 in range).
        carrier
            .items
            .push(QueryItem::RangeAfter(b"brand_000".to_vec()..));
        carrier.set_subquery_path(vec![b"color".to_vec()]);
        carrier.set_subquery(Query::new_aggregate_count_on_range(QueryItem::RangeAfter(
            b"color_00049".to_vec()..,
        )));
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"byBrand".to_vec()],
            SizedQuery::new(carrier, Some(2), None),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query (carrier with Range outer + limit) should succeed");
        let (got_root, results) =
            GroveDb::verify_aggregate_count_query_per_key(&proof, &path_query, v)
                .expect("verify carrier with Range outer + limit should succeed");
        assert_eq!(got_root, expected_root);
        assert_eq!(
            results.len(),
            2,
            "expected exactly `limit` outer matches from the range walk"
        );
        // RangeAfter("brand_000") + left_to_right=true: first two
        // matches in ascending lex order.
        assert_eq!(results[0].0, b"brand_001".to_vec());
        assert_eq!(results[1].0, b"brand_002".to_vec());
        for (_, count) in &results {
            assert_eq!(*count, 50);
        }
    }

    #[test]
    fn carrier_range_outer_with_limit_zero_returns_no_results() {
        // limit=0 caps the outer walk to zero matches. The proof still
        // verifies (it commits to "no outer matches walked"), and the
        // result vector is empty.
        use grovedb_query::Query;
        let v = GroveVersion::latest();
        let (db, expected_root) =
            setup_brand_color_carrier_tree(v, &[b"brand_000", b"brand_001"], 100);

        let mut carrier = Query::new();
        carrier
            .items
            .push(QueryItem::RangeAfter(b"brand_000".to_vec()..));
        carrier.set_subquery_path(vec![b"color".to_vec()]);
        carrier.set_subquery(Query::new_aggregate_count_on_range(QueryItem::RangeAfter(
            b"color_00049".to_vec()..,
        )));
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"byBrand".to_vec()],
            SizedQuery::new(carrier, Some(0), None),
        );

        // The v0 generate.rs entry-point gate rejects `limit == 0` for
        // proved queries unconditionally (it's been a long-standing
        // rule that "proved path queries can not be for limit 0"). The
        // no-proof per-key entry point, however, accepts limit=0 and
        // honors it as "walk zero outer matches" — exercise that here
        // since it's the path callers would use to dry-run the shape.
        let no_proof = db
            .grove_db
            .query_aggregate_count_per_key(&path_query, None, v)
            .unwrap()
            .expect("no-proof per-key with limit=0 should succeed");
        assert!(no_proof.is_empty(), "limit=0 must produce zero results");

        // The expected root is unaffected by the no-proof walk; assert
        // we haven't accidentally produced any side effects.
        let root = db.grove_db.root_hash(None, v).unwrap().expect("root_hash");
        assert_eq!(root, expected_root);
    }

    #[test]
    fn carrier_range_outer_with_limit_exceeding_available_walks_all() {
        // Limit set higher than the number of in-range outer keys: the
        // walk produces all available matches and behaves identically
        // to a query with no limit set.
        use grovedb_query::Query;
        let v = GroveVersion::latest();
        let (db, expected_root) =
            setup_brand_color_carrier_tree(v, &[b"brand_000", b"brand_001", b"brand_002"], 100);

        let mut carrier_with_limit = Query::new();
        carrier_with_limit
            .items
            .push(QueryItem::RangeAfter(b"brand_000".to_vec()..));
        carrier_with_limit.set_subquery_path(vec![b"color".to_vec()]);
        carrier_with_limit.set_subquery(Query::new_aggregate_count_on_range(
            QueryItem::RangeAfter(b"color_00049".to_vec()..),
        ));
        let path_query_with_limit = PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"byBrand".to_vec()],
            // Only 2 brands are in range (brand_001, brand_002); ask
            // for up to 100.
            SizedQuery::new(carrier_with_limit, Some(100), None),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query_with_limit, None, v)
            .unwrap()
            .expect("prove_query with oversized limit should succeed");
        let (got_root, results) =
            GroveDb::verify_aggregate_count_query_per_key(&proof, &path_query_with_limit, v)
                .expect("verify with oversized limit should succeed");
        assert_eq!(got_root, expected_root);
        assert_eq!(results.len(), 2, "all in-range outer keys returned");
        assert_eq!(results[0].0, b"brand_001".to_vec());
        assert_eq!(results[1].0, b"brand_002".to_vec());

        // And the per-key no-proof walk agrees.
        let no_proof = db
            .grove_db
            .query_aggregate_count_per_key(&path_query_with_limit, None, v)
            .unwrap()
            .expect("no-proof per-key with oversized limit should succeed");
        assert_eq!(no_proof, results);
    }

    /// Root-carrier regression: a carrier `AggregateCountOnRange` query
    /// with an empty `PathQuery::path` must validate and round-trip
    /// correctly. The shape-aware empty-path fix in the auto-dispatcher
    /// allows carriers to fan out at the root layer while still
    /// rejecting leaf-shape count queries at the root.
    #[test]
    fn root_carrier_count_with_empty_path_succeeds() {
        use grovedb_query::Query;
        let v = GroveVersion::latest();
        let db = crate::tests::make_test_grovedb(v);
        for leaf in [TEST_LEAF, b"test_leaf2"] {
            db.insert(
                [leaf].as_ref(),
                b"ct",
                Element::empty_provable_count_tree(),
                None,
                None,
                v,
            )
            .unwrap()
            .expect("insert ct");
            for c in b'a'..=b'e' {
                db.insert(
                    [leaf, b"ct"].as_ref(),
                    &[c],
                    Element::new_item(b"v".to_vec()),
                    None,
                    None,
                    v,
                )
                .unwrap()
                .expect("insert count item");
            }
        }
        let expected_root = db.grove_db.root_hash(None, v).unwrap().expect("root_hash");

        let mut carrier = Query::new();
        carrier.insert_key(TEST_LEAF.to_vec());
        carrier.insert_key(b"test_leaf2".to_vec());
        carrier.set_subquery_path(vec![b"ct".to_vec()]);
        carrier.set_subquery(Query::new_aggregate_count_on_range(QueryItem::RangeFrom(
            b"a".to_vec()..,
        )));
        let path_query = PathQuery::new(Vec::new(), SizedQuery::new(carrier, None, None));

        path_query
            .validate_aggregate_count_on_range()
            .expect("root-carrier ACOR must validate");

        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove root-carrier ACOR");
        let (got_root, results) =
            GroveDb::verify_aggregate_count_query_per_key(&proof, &path_query, v)
                .expect("verify root-carrier ACOR");
        assert_eq!(got_root, expected_root, "root must match GroveDB root");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, TEST_LEAF.to_vec());
        assert_eq!(results[1].0, b"test_leaf2".to_vec());
        // Each leaf ProvableCountTree holds 5 items.
        assert_eq!(results[0].1, 5);
        assert_eq!(results[1].1, 5);
    }

    /// Leaf `AggregateCountOnRange` at empty path is STILL rejected —
    /// the shape-aware relaxation only applies to carriers.
    #[test]
    fn root_leaf_count_with_empty_path_still_rejected() {
        let v = GroveVersion::latest();
        let _db = crate::tests::make_test_grovedb(v);
        let pq = PathQuery::new_aggregate_count_on_range(
            Vec::new(),
            QueryItem::RangeFrom(b"a".to_vec()..),
        );
        let err = pq
            .validate_aggregate_count_on_range()
            .expect_err("leaf at empty path must still be rejected");
        let msg = format!("{err}");
        assert!(
            msg.contains("leaf") && msg.contains("ProvableCountTree"),
            "expected leaf-only rejection message, got: {msg}"
        );
        let dummy = vec![0u8; 4];
        assert!(GroveDb::verify_aggregate_count_query(&dummy, &pq, v).is_err());
    }
}
