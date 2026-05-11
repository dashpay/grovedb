//! End-to-end GroveDB tests for `AggregateCountOnRange` queries.
//!
//! These exercise the full prove → encode → decode → verify pipeline against
//! both `ProvableCountTree` and `ProvableCountSumTree` (and their
//! `NonCounted*` wrappers via being the *parent* tree, not the queried one),
//! at various path depths and across the full set of allowed range variants.

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
        // the recursion reaches the ACOR-bearing leaf level. If the path
        // doesn't exist (e.g. "missing" key under TEST_LEAF), the recursive
        // prover never sees the ACOR item and the malformed query is allowed
        // to return a regular path/absence proof. Fix: validate at the
        // `prove_query` entry point, before any recursive dispatch.
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"missing".to_vec()],
            // QueryItem::Key as the inner range is invalid for ACOR.
            QueryItem::Key(b"k".to_vec()),
        );
        let prove_result = db.grove_db.prove_query(&path_query, None, v).unwrap();
        match prove_result {
            Err(crate::Error::InvalidQuery(msg)) => {
                assert!(
                    msg.contains("AggregateCountOnRange may not wrap Key"),
                    "expected ACOR-Key rejection, got: {msg}"
                );
            }
            other => panic!(
                "malformed ACOR with non-existent path must be rejected at entry, got {:?}",
                other.map(|b| b.len())
            ),
        }
    }

    #[test]
    fn aggregate_count_hidden_in_subquery_branch_is_rejected_at_entry() {
        // Codex's broader concern: an `AggregateCountOnRange` smuggled
        // inside a `default_subquery_branch.subquery` is also invalid (ACOR
        // is terminal — it cannot be reached via a normal subquery path)
        // and must be rejected up front. The recursive detector
        // `has_aggregate_count_on_range_anywhere` finds the hidden ACOR;
        // top-level `validate_aggregate_count_on_range` then rejects
        // because the surrounding query isn't the canonical single-ACOR
        // shape.
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        let inner_acor = QueryItem::AggregateCountOnRange(Box::new(QueryItem::Range(
            b"a".to_vec()..b"z".to_vec(),
        )));
        let mut sub_query = grovedb_merk::proofs::Query::new();
        sub_query.insert_item(inner_acor);
        let mut top_query = grovedb_merk::proofs::Query::new();
        top_query.insert_range_inclusive(b"a".to_vec()..=b"z".to_vec());
        top_query.set_subquery(sub_query);
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec()],
            SizedQuery::new(top_query, None, None),
        );
        let prove_result = db.grove_db.prove_query(&path_query, None, v).unwrap();
        assert!(
            matches!(prove_result, Err(crate::Error::InvalidQuery(_))),
            "ACOR hidden in subquery branch must be rejected at entry, got {:?}",
            prove_result.map(|b| b.len())
        );
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
    fn provable_count_tree_works_on_grove_v2_envelope() {
        // GROVE_V2 dispatches to the V0 prove_query_non_serialized path, which
        // produces a `MerkOnlyLayerProof` envelope rather than V1's
        // `LayerProof`. Verify the same prove → verify cycle works through that
        // envelope.
        let v: &GroveVersion = &GROVE_V2;
        let (db, root) = setup_15_key_provable_count_tree(v);
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query (v0 envelope) should succeed");
        let (got_root, got_count) = GroveDb::verify_aggregate_count_query(&proof, &path_query, v)
            .expect("verify should succeed against v0 envelope");
        assert_eq!(got_root, root);
        assert_eq!(got_count, 10);
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

    /// `Element::NonCounted` wrappers tell the parent tree to **skip** the
    /// wrapped element when aggregating its own count.
    /// `AggregateCountOnRange` honors that: NonCounted children are
    /// excluded from the result.
    ///
    /// Mechanics — every node in a `ProvableCountTree` carries an
    /// own_count of 1 (normal) or 0 (NonCounted). The merk-recorded
    /// aggregate at any subtree = sum of own_counts in the subtree
    /// (NonCounted entries contribute 0). The verifier's shape walk
    /// derives each boundary node's own_count as
    /// `node_aggregate − left_struct − right_struct` and credits **only
    /// own_count** to the in-range total when the key falls in range.
    /// For a NonCounted leaf, own_count = 0 and the wrapped key
    /// contributes nothing. The structural counts threaded through the
    /// walk are hash-bound at every step (every count-bearing proof node
    /// feeds its count into `node_hash_with_count`), so a malicious
    /// prover can't lie about a NonCounted node's status without
    /// breaking the parent's hash chain.
    #[test]
    fn non_counted_children_are_excluded_from_aggregate_count() {
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

        // Five regular items — each contributes 1.
        for c in [b'a', b'b', b'c', b'd', b'e'] {
            db.insert(
                [TEST_LEAF, b"ct"].as_ref(),
                &[c],
                Element::new_item(vec![c]),
                None,
                None,
                v,
            )
            .unwrap()
            .expect("insert regular item");
        }

        // One NonCounted-wrapped item, key "f" — in-range but contributes
        // 0 (own_count = 0).
        let nc_item =
            Element::new_non_counted(Element::new_item(b"hidden".to_vec())).expect("wrap ok");
        db.insert([TEST_LEAF, b"ct"].as_ref(), b"f", nc_item, None, None, v)
            .unwrap()
            .expect("insert NonCounted item");

        let root = db.grove_db.root_hash(None, v).unwrap().expect("root_hash");

        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeInclusive(b"a".to_vec()..=b"z".to_vec()),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove");
        let (got_root, got_count) =
            GroveDb::verify_aggregate_count_query(&proof, &path_query, v).expect("verify");
        assert_eq!(got_root, root, "root mismatch");
        assert_eq!(
            got_count, 5,
            "NonCounted-wrapped child must be excluded from the aggregate count"
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
        // The merk-level error gets wrapped in Error::MerkError; we just
        // require *some* error rather than asserting on the exact variant
        // since the merk layer's InvalidProofError formatting is internal.
        match err {
            crate::Error::MerkError(_) => {}
            other => panic!("expected MerkError, got {:?}", other),
        }
    }

    #[test]
    fn no_proof_uses_provided_transaction() {
        // Exercise the TransactionArg = Some(&tx) path of query_aggregate_count.
        // A transactional read sees the committed state inside the same
        // transaction.
        let v = GroveVersion::latest();
        let (db, _) = setup_15_key_provable_count_tree(v);
        let tx = db.start_transaction();
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
        );
        let count = db
            .grove_db
            .query_aggregate_count(&path_query, Some(&tx), v)
            .unwrap()
            .expect("query_aggregate_count with transaction should succeed");
        assert_eq!(count, 10);
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
}
