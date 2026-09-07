//! Runtime validation for audit finding P14 (issue #862): a V1 proof
//! must never let raw reference-family element bytes reach the caller or
//! steer a descent decision, because no accepted proof node form binds
//! them.
//!
//! An honest V1 prover never emits a raw reference. Every reference row
//! is rewritten by the GroveDB post-pass into a `KVRefValueHash*` node
//! that carries the dereferenced TARGET bytes (hashed into the node) and
//! only the *hash* of the reference element. But a `KVValueHash` /
//! `KVValueHashFeatureType` node hashes `(key, value_hash)` alone, and
//! the merk-level V1 guard added by PR #553 rejects only *item* elements
//! there. So an attacker can take the honest `KVRefValueHash(key, target,
//! H(ref))` node, recompute the combined value hash it commits to, and
//! re-present the row as `KVValueHash(key, <forged reference bytes>,
//! combined)`: the Merk root still reconstructs, the guard passes (the
//! bytes are a reference, not an item), and the GroveDB layer sees a
//! `Reference` element whose path / type it never authenticated.
//!
//! Two exploit shapes are exercised here, each one an actual attack
//! against the live `develop` verifier before the fix:
//!
//! 1. **Forged result row** — the verifier returns the forged reference
//!    (attacker-chosen target path) as the proven element for the key.
//! 2. **Silent descent skip** — a populated subtree the query descends
//!    into is re-presented as a reference; every branch of the terminal
//!    arm falls through without error, the lower layer is dropped, and
//!    the proof verifies with the subtree's rows missing.

#[cfg(test)]
mod tests {
    use std::collections::LinkedList;

    use grovedb_merk::{
        proofs::{encode_into, Decoder, Node, Op},
        tree::{combine_hash, value_hash, TreeFeatureType},
    };
    use grovedb_version::version::GroveVersion;

    use crate::{
        operations::proof::{GroveDBProof, GroveDBProofV1, LayerProof, ProofBytes},
        reference_path::ReferencePathType,
        tests::{make_test_grovedb, TempGroveDb, ANOTHER_TEST_LEAF, TEST_LEAF},
        Element, Error, GroveDb, PathQuery, Query, SizedQuery,
    };

    const TARGET_KEY: &[u8] = b"target";
    const DECOY_KEY: &[u8] = b"decoy";
    const REF_KEY: &[u8] = b"ref";
    const COUNT_TREE_KEY: &[u8] = b"pct";
    const SUB_KEY: &[u8] = b"sub";

    // =================================================================
    // Envelope helpers
    // =================================================================

    fn proof_cfg() -> impl bincode::config::Config {
        bincode::config::standard()
            .with_big_endian()
            .with_no_limit()
    }

    fn decode_v1_root(proof: &[u8]) -> LayerProof {
        let (decoded, consumed) = bincode::decode_from_slice::<GroveDBProof, _>(proof, proof_cfg())
            .expect("decode proof");
        assert_eq!(consumed, proof.len(), "proof must decode canonically");
        match decoded {
            GroveDBProof::V1(GroveDBProofV1 { root_layer }) => root_layer,
            GroveDBProof::V0(_) => panic!("expected a V1 proof envelope"),
        }
    }

    fn encode_v1_root(root_layer: LayerProof) -> Vec<u8> {
        bincode::encode_to_vec(GroveDBProof::V1(GroveDBProofV1 { root_layer }), proof_cfg())
            .expect("encode tampered envelope")
    }

    fn merk_bytes(layer: &LayerProof) -> Vec<u8> {
        match &layer.merk_proof {
            ProofBytes::Merk(b) => b.clone(),
            _ => panic!("layer under forge must carry ProofBytes::Merk"),
        }
    }

    /// Re-present the honest `KVRefValueHash{,Count}` node for
    /// `target_key` as a `KVValueHash` / `KVValueHashFeatureType` node
    /// carrying `forged_value` and the *same* committed value hash the
    /// honest node reconstructs — `combine_hash(H(reference), H(target))`.
    /// The Merk chain is unchanged by construction.
    fn forge_reference_row(layer: &mut LayerProof, target_key: &[u8], forged_value: Vec<u8>) {
        let original = merk_bytes(layer);
        let mut ops: LinkedList<Op> = LinkedList::new();
        let mut replaced = false;
        let mut seen = Vec::new();
        for op in Decoder::new(&original) {
            let op = op.expect("decode proof op");
            let mut rewrite = |node: Node| -> Node {
                match node {
                    Node::KVRefValueHash(k, referenced, ref_hash) => {
                        seen.push(hex::encode(&k));
                        if k.as_slice() == target_key {
                            replaced = true;
                            let combined =
                                combine_hash(&ref_hash, &value_hash(&referenced).unwrap()).unwrap();
                            Node::KVValueHash(k, forged_value.clone(), combined)
                        } else {
                            Node::KVRefValueHash(k, referenced, ref_hash)
                        }
                    }
                    Node::KVRefValueHashCount(k, referenced, ref_hash, count) => {
                        seen.push(hex::encode(&k));
                        if k.as_slice() == target_key {
                            replaced = true;
                            let combined =
                                combine_hash(&ref_hash, &value_hash(&referenced).unwrap()).unwrap();
                            Node::KVValueHashFeatureType(
                                k,
                                forged_value.clone(),
                                combined,
                                TreeFeatureType::ProvableCountedMerkNode(count),
                            )
                        } else {
                            Node::KVRefValueHashCount(k, referenced, ref_hash, count)
                        }
                    }
                    other => other,
                }
            };
            let new_op = match op {
                Op::Push(node) => Op::Push(rewrite(node)),
                Op::PushInverted(node) => Op::PushInverted(rewrite(node)),
                other => other,
            };
            ops.push_back(new_op);
        }
        assert!(
            replaced,
            "forge: key {} not found as a KVRefValueHash{{,Count}} node (saw {:?})",
            hex::encode(target_key),
            seen
        );
        let mut out = Vec::with_capacity(original.len() + forged_value.len());
        encode_into(ops.iter(), &mut out);
        layer.merk_proof = ProofBytes::Merk(out);
    }

    /// Swap only the value bytes of the `KVValueHash*` node for
    /// `target_key`, keeping the committed value hash. Used to
    /// re-present a populated subtree as a reference.
    fn forge_value_bytes(layer: &mut LayerProof, target_key: &[u8], forged_value: Vec<u8>) {
        let original = merk_bytes(layer);
        let mut ops: LinkedList<Op> = LinkedList::new();
        let mut replaced = false;
        let mut seen = Vec::new();
        for op in Decoder::new(&original) {
            let op = op.expect("decode proof op");
            let mut rewrite = |node: Node| -> Node {
                match node {
                    Node::KVValueHash(k, v, vh) => {
                        seen.push(hex::encode(&k));
                        if k.as_slice() == target_key {
                            replaced = true;
                            Node::KVValueHash(k, forged_value.clone(), vh)
                        } else {
                            Node::KVValueHash(k, v, vh)
                        }
                    }
                    Node::KVValueHashFeatureType(k, v, vh, ft) => {
                        seen.push(hex::encode(&k));
                        if k.as_slice() == target_key {
                            replaced = true;
                            Node::KVValueHashFeatureType(k, forged_value.clone(), vh, ft)
                        } else {
                            Node::KVValueHashFeatureType(k, v, vh, ft)
                        }
                    }
                    other => other,
                }
            };
            let new_op = match op {
                Op::Push(node) => Op::Push(rewrite(node)),
                Op::PushInverted(node) => Op::PushInverted(rewrite(node)),
                other => other,
            };
            ops.push_back(new_op);
        }
        assert!(
            replaced,
            "forge: key {} not found as a KVValueHash{{FeatureType}} node (saw {:?})",
            hex::encode(target_key),
            seen
        );
        let mut out = Vec::with_capacity(original.len() + forged_value.len());
        encode_into(ops.iter(), &mut out);
        layer.merk_proof = ProofBytes::Merk(out);
    }

    fn assert_rejected_as_unbound_reference(result: Result<impl std::fmt::Debug, Error>) {
        match result {
            Err(Error::InvalidProof(_, msg)) => assert!(
                msg.to_ascii_lowercase().contains("reference"),
                "rejection must name the unbound reference row; got {msg}"
            ),
            other => panic!("forged reference row must be rejected as InvalidProof; got {other:?}"),
        }
    }

    // =================================================================
    // Fixtures
    // =================================================================

    /// `[TEST_LEAF]/target` and `[TEST_LEAF]/decoy` are items;
    /// `[ANOTHER_TEST_LEAF]/ref` is a reference to `target`.
    fn reference_fixture(grove_version: &GroveVersion) -> TempGroveDb {
        let db = make_test_grovedb(grove_version);
        db.insert(
            &[TEST_LEAF],
            TARGET_KEY,
            Element::new_item(b"the real target".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert target");
        db.insert(
            &[TEST_LEAF],
            DECOY_KEY,
            Element::new_item(b"an unrelated element".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert decoy");
        db.insert(
            &[ANOTHER_TEST_LEAF],
            REF_KEY,
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                TARGET_KEY.to_vec(),
            ])),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert reference");
        db
    }

    fn forged_reference_bytes(grove_version: &GroveVersion) -> Vec<u8> {
        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            DECOY_KEY.to_vec(),
        ]))
        .serialize(grove_version)
        .expect("serialize forged reference")
    }

    fn key_query(path: Vec<Vec<u8>>, key: &[u8]) -> PathQuery {
        let mut q = Query::new();
        q.insert_key(key.to_vec());
        PathQuery::new(path, SizedQuery::new(q, None, None))
    }

    // =================================================================
    // 1. Forged result row through a bare KVValueHash node
    // =================================================================

    #[test]
    fn honest_reference_proof_returns_the_dereferenced_target() {
        let grove_version = GroveVersion::latest();
        let db = reference_fixture(grove_version);
        let path_query = key_query(vec![ANOTHER_TEST_LEAF.to_vec()], REF_KEY);
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove");
        let (root, rows) = GroveDb::verify_query(&proof, &path_query, grove_version)
            .expect("honest proof verifies");
        assert_eq!(root, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(
            rows,
            vec![(
                vec![ANOTHER_TEST_LEAF.to_vec()],
                REF_KEY.to_vec(),
                Some(Element::new_item(b"the real target".to_vec()))
            )]
        );
    }

    #[test]
    fn forged_reference_row_in_kv_value_hash_node_is_rejected() {
        let grove_version = GroveVersion::latest();
        let db = reference_fixture(grove_version);
        let path_query = key_query(vec![ANOTHER_TEST_LEAF.to_vec()], REF_KEY);
        let honest = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove");

        let mut root_layer = decode_v1_root(&honest);
        let layer = root_layer
            .lower_layers
            .get_mut(ANOTHER_TEST_LEAF)
            .expect("layer for the reference's tree");
        forge_reference_row(layer, REF_KEY, forged_reference_bytes(grove_version));
        let tampered = encode_v1_root(root_layer);

        assert_rejected_as_unbound_reference(GroveDb::verify_query(
            &tampered,
            &path_query,
            grove_version,
        ));
        assert_rejected_as_unbound_reference(GroveDb::verify_query_raw(
            &tampered,
            &path_query,
            grove_version,
        ));
    }

    /// Same forgery when the reference is wrapped in a count-carrying
    /// parent: the honest `KVRefValueHashCount` becomes a
    /// `KVValueHashFeatureType` carrying the honest count.
    #[test]
    fn forged_reference_row_in_feature_type_node_is_rejected() {
        let grove_version = GroveVersion::latest();
        let db = reference_fixture(grove_version);
        db.insert(
            &[TEST_LEAF],
            COUNT_TREE_KEY,
            Element::empty_provable_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert count tree");
        db.insert(
            &[TEST_LEAF, COUNT_TREE_KEY],
            REF_KEY,
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                TARGET_KEY.to_vec(),
            ])),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert reference under count tree");

        let path_query = key_query(vec![TEST_LEAF.to_vec(), COUNT_TREE_KEY.to_vec()], REF_KEY);
        let honest = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove");
        let (_, rows) = GroveDb::verify_query(&honest, &path_query, grove_version)
            .expect("honest proof verifies");
        assert_eq!(
            rows[0].2,
            Some(Element::new_item(b"the real target".to_vec()))
        );

        let mut root_layer = decode_v1_root(&honest);
        let layer = root_layer
            .lower_layers
            .get_mut(TEST_LEAF)
            .and_then(|l| l.lower_layers.get_mut(COUNT_TREE_KEY))
            .expect("layer for the count tree");
        forge_reference_row(layer, REF_KEY, forged_reference_bytes(grove_version));
        let tampered = encode_v1_root(root_layer);

        assert_rejected_as_unbound_reference(GroveDb::verify_query(
            &tampered,
            &path_query,
            grove_version,
        ));
    }

    /// The forged bytes may also be a `ReferenceWithSumItem`; the
    /// family, not just the plain variant, is unbound.
    #[test]
    fn forged_reference_with_sum_item_row_is_rejected() {
        let grove_version = GroveVersion::latest();
        let db = reference_fixture(grove_version);
        let path_query = key_query(vec![ANOTHER_TEST_LEAF.to_vec()], REF_KEY);
        let honest = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove");

        let forged = Element::new_reference_with_sum_item(
            ReferencePathType::AbsolutePathReference(vec![TEST_LEAF.to_vec(), DECOY_KEY.to_vec()]),
            7,
        )
        .serialize(grove_version)
        .expect("serialize forged reference with sum item");

        let mut root_layer = decode_v1_root(&honest);
        let layer = root_layer
            .lower_layers
            .get_mut(ANOTHER_TEST_LEAF)
            .expect("layer for the reference's tree");
        forge_reference_row(layer, REF_KEY, forged);
        let tampered = encode_v1_root(root_layer);

        assert_rejected_as_unbound_reference(GroveDb::verify_query(
            &tampered,
            &path_query,
            grove_version,
        ));
    }

    // =================================================================
    // 2. Silent descent skip: a populated subtree re-presented as a
    //    reference, lower layer dropped.
    // =================================================================

    fn subtree_fixture(grove_version: &GroveVersion) -> TempGroveDb {
        let db = make_test_grovedb(grove_version);
        db.insert(
            &[TEST_LEAF],
            SUB_KEY,
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert subtree");
        for (k, v) in [(b"a", b"1"), (b"b", b"2"), (b"c", b"3")] {
            db.insert(
                &[TEST_LEAF, SUB_KEY],
                k.as_slice(),
                Element::new_item(v.to_vec()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert subtree row");
        }
        db
    }

    fn subtree_query() -> PathQuery {
        let mut inner = Query::new();
        inner.insert_all();
        let mut q = Query::new();
        q.insert_key(SUB_KEY.to_vec());
        q.set_subquery(inner);
        PathQuery::new(vec![TEST_LEAF.to_vec()], SizedQuery::new(q, None, None))
    }

    #[test]
    fn forged_reference_cannot_hide_a_populated_subtree() {
        let grove_version = GroveVersion::latest();
        let db = subtree_fixture(grove_version);
        let path_query = subtree_query();
        let honest = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove");
        let (_, rows) = GroveDb::verify_query(&honest, &path_query, grove_version)
            .expect("honest proof verifies");
        assert_eq!(rows.len(), 3, "the honest proof carries every subtree row");

        // Re-present the subtree element as a reference and drop the
        // lower layer the query would have descended into.
        let mut root_layer = decode_v1_root(&honest);
        let layer = root_layer
            .lower_layers
            .get_mut(TEST_LEAF)
            .expect("layer for TEST_LEAF");
        forge_value_bytes(layer, SUB_KEY, forged_reference_bytes(grove_version));
        assert!(
            layer.lower_layers.remove(SUB_KEY).is_some(),
            "honest proof descends into the subtree"
        );
        let tampered = encode_v1_root(root_layer);

        assert_rejected_as_unbound_reference(GroveDb::verify_query(
            &tampered,
            &path_query,
            grove_version,
        ));
    }
}
