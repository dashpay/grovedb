//! Trunk / branch chunk proofs: composite rows must be bound to the root
//! hash (#859).
//!
//! A merk chunk proof binds an item row through `KV` (the verifier hashes the
//! value bytes itself) but a tree or reference row through `KVValueHash` /
//! `KVValueHashFeatureType`, whose `value_hash` the merk verifier treats as
//! opaque. Nothing recomputes the tree's `combine_hash(H(value), child_root)`
//! composition, so the returned element bytes — tree type, aggregate count or
//! sum, root key — were free for a prover to forge under a genuine root hash.
//! From `GROVE_V4` the prover rewrites every composite row to
//! `KVValueHashFeatureTypeWithChildHash` and the verifier requires it.

#[cfg(test)]
mod tests {
    use blake3::Hasher;
    use grovedb_merk::{
        proofs::{encode_into, Decoder, Node, Op},
        tree::{combine_hash, value_hash},
        TreeFeatureType,
    };
    use grovedb_version::version::{v1::GROVE_V1, v3::GROVE_V3, GroveVersion};

    use crate::{
        operations::proof::{GroveDBProof, GroveDBProofV1, ProofBytes},
        query::{PathBranchChunkQuery, PathTrunkChunkQuery},
        reference_path::ReferencePathType,
        tests::{common::EMPTY_PATH, make_empty_grovedb, TempGroveDb},
        Element, Error, GroveDb,
    };

    fn bincode_config() -> bincode::config::Configuration<
        bincode::config::BigEndian,
        bincode::config::Varint,
        bincode::config::NoLimit,
    > {
        bincode::config::standard()
            .with_big_endian()
            .with_no_limit()
    }

    fn decode_v1(proof: &[u8]) -> GroveDBProofV1 {
        let decoded: GroveDBProof = bincode::decode_from_slice(proof, bincode_config())
            .expect("decode proof")
            .0;
        match decoded {
            GroveDBProof::V1(v1) => v1,
            GroveDBProof::V0(_) => panic!("expected a V1 trunk proof"),
        }
    }

    fn target_layer_ops(proof_v1: &GroveDBProofV1, key: &[u8]) -> Vec<Op> {
        let layer = proof_v1
            .root_layer
            .lower_layers
            .get(key)
            .expect("target layer");
        let ProofBytes::Merk(bytes) = &layer.merk_proof else {
            panic!("expected Merk bytes at the target layer");
        };
        Decoder::new(bytes)
            .collect::<Result<Vec<_>, _>>()
            .expect("decode ops")
    }

    fn rebuild_with_target_ops(mut proof_v1: GroveDBProofV1, key: &[u8], ops: &[Op]) -> Vec<u8> {
        let mut encoded = Vec::new();
        encode_into(ops.iter(), &mut encoded);
        proof_v1
            .root_layer
            .lower_layers
            .get_mut(key)
            .expect("target layer")
            .merk_proof = ProofBytes::Merk(encoded);
        bincode::encode_to_vec(GroveDBProof::V1(proof_v1), bincode_config()).expect("encode")
    }

    fn encode_ops(ops: &[Op]) -> Vec<u8> {
        let mut encoded = Vec::new();
        encode_into(ops.iter(), &mut encoded);
        encoded
    }

    /// A forged tree element: a different tree type, a large aggregate
    /// count, and a root key that does not exist.
    fn forged_tree_bytes(grove_version: &GroveVersion) -> Vec<u8> {
        Element::CountTree(Some(b"phantom".to_vec()), 1_000_000, None)
            .serialize(grove_version)
            .expect("serialize forged tree")
    }

    fn node_of(op: &Op) -> Option<&Node> {
        match op {
            Op::Push(node) | Op::PushInverted(node) => Some(node),
            _ => None,
        }
    }

    /// Downgrade attack: strip the child hash from the first bound composite
    /// row and substitute forged bytes, keeping the committed `value_hash`
    /// (and feature type) so the merk root hash is unchanged.
    fn downgrade_first_bound_row(ops: &mut [Op], forged: &[u8]) -> Option<Vec<u8>> {
        for op in ops.iter_mut() {
            if let Op::Push(Node::KVValueHashFeatureTypeWithChildHash(key, _, vh, ft, _)) = op {
                let key = key.clone();
                let replacement = if *ft == TreeFeatureType::BasicMerkNode {
                    Node::KVValueHash(key.clone(), forged.to_vec(), *vh)
                } else {
                    Node::KVValueHashFeatureType(key.clone(), forged.to_vec(), *vh, *ft)
                };
                *op = Op::Push(replacement);
                return Some(key);
            }
        }
        None
    }

    /// In-place attack: keep the bound node form, swap the value bytes.
    fn forge_first_bound_row_in_place(ops: &mut [Op], forged: &[u8]) -> Option<Vec<u8>> {
        for op in ops.iter_mut() {
            if let Op::Push(Node::KVValueHashFeatureTypeWithChildHash(key, value, ..)) = op {
                *value = forged.to_vec();
                return Some(key.clone());
            }
        }
        None
    }

    /// Disguise attack: present an item row as a tree, keeping the item's
    /// committed `value_hash` (`H(item)`) so the merk root is unchanged.
    fn disguise_first_item_as_tree(ops: &mut [Op], forged: &[u8]) -> Option<Vec<u8>> {
        for op in ops.iter_mut() {
            if let Op::Push(Node::KV(key, value)) = op {
                let real_vh = value_hash(value).unwrap();
                let key = key.clone();
                *op = Op::Push(Node::KVValueHash(key.clone(), forged.to_vec(), real_vh));
                return Some(key);
            }
        }
        None
    }

    fn assert_rejected_unbound(result: Result<impl std::fmt::Debug, Error>, what: &str) {
        match result {
            Ok(returned) => panic!("{what} verified; returned {returned:?}"),
            Err(e) => {
                let msg = format!("{e:?}");
                assert!(
                    msg.contains("must carry its child hash")
                        || msg.contains("value/child hash mismatch")
                        || msg.contains("value hash mismatch"),
                    "{what}: unexpected rejection: {msg}"
                );
            }
        }
    }

    /// Count tree host with five items and three subtrees (one populated).
    fn count_tree_with_subtrees(grove_version: &GroveVersion, host: Element) -> TempGroveDb {
        let db = make_empty_grovedb();
        db.insert(EMPTY_PATH, b"ct", host, None, None, grove_version)
            .unwrap()
            .expect("insert host");
        for i in 0u8..5 {
            db.insert(
                &[b"ct"],
                format!("item_{i}").as_bytes(),
                Element::new_item(vec![i; 8]),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert item");
        }
        for i in 0u8..3 {
            db.insert(
                &[b"ct"],
                format!("sub_{i}").as_bytes(),
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert subtree");
        }
        db.insert(
            &[b"ct".as_slice(), b"sub_0".as_slice()],
            b"inner",
            Element::new_item(vec![7]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert inner item");
        db
    }

    fn trunk_query() -> PathTrunkChunkQuery {
        PathTrunkChunkQuery::new(vec![b"ct".to_vec()], 4)
    }

    fn prove_trunk(db: &TempGroveDb, grove_version: &GroveVersion) -> Vec<u8> {
        db.prove_trunk_chunk(&trunk_query(), grove_version)
            .unwrap()
            .expect("prove trunk")
    }

    // ── Honest proofs ──────────────────────────────────────────────────

    #[test]
    fn trunk_v4_binds_every_tree_row_and_returns_stored_elements() {
        let grove_version = GroveVersion::latest();
        let db = count_tree_with_subtrees(grove_version, Element::empty_count_tree());

        let proof = prove_trunk(&db, grove_version);
        let (_, result) = GroveDb::verify_trunk_chunk_proof(&proof, &trunk_query(), grove_version)
            .expect("honest proof verifies");

        let ops = target_layer_ops(&decode_v1(&proof), b"ct");
        let mut bound_tree_rows = 0;
        for node in ops.iter().filter_map(node_of) {
            match node {
                Node::KVValueHashFeatureTypeWithChildHash(key, value, vh, _, child) => {
                    let element = Element::deserialize(value, grove_version).unwrap();
                    assert!(
                        element.is_any_tree(),
                        "bound row {} is not a tree",
                        hex::encode(key)
                    );
                    assert_eq!(
                        combine_hash(&value_hash(value).unwrap(), child).unwrap(),
                        *vh,
                        "child hash must reproduce the committed value hash"
                    );
                    bound_tree_rows += 1;
                }
                Node::KVValueHash(key, ..) | Node::KVValueHashFeatureType(key, ..) => {
                    panic!("row {} was left unbound under GROVE_V4", hex::encode(key));
                }
                _ => {}
            }
        }
        assert_eq!(
            bound_tree_rows, 3,
            "all three subtrees are within the trunk"
        );

        // Returned elements are the stored ones: sub_0 is populated, the
        // other two are empty.
        assert!(matches!(
            result.elements.get(b"sub_0".as_slice()),
            Some(Element::Tree(Some(_), _))
        ));
        assert!(matches!(
            result.elements.get(b"sub_1".as_slice()),
            Some(Element::Tree(None, _))
        ));
        assert_eq!(result.elements.len(), 8);
    }

    #[test]
    fn honest_chunk_proofs_with_reference_rows_verify_under_v4() {
        let grove_version = GroveVersion::latest();
        let db = count_tree_with_subtrees(grove_version, Element::empty_count_tree());
        db.insert(
            EMPTY_PATH,
            b"target",
            Element::new_item(b"referenced".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert target");
        db.insert(
            &[b"ct"],
            b"ref",
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                b"target".to_vec()
            ])),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert reference");

        let proof = prove_trunk(&db, grove_version);
        let (_, result) = GroveDb::verify_trunk_chunk_proof(&proof, &trunk_query(), grove_version)
            .expect("honest trunk proof with a reference row verifies");
        assert!(
            matches!(
                result.elements.get(b"ref".as_slice()),
                Some(Element::Reference(..))
            ),
            "reference row should be returned"
        );

        let ops = target_layer_ops(&decode_v1(&proof), b"ct");
        let ref_row = ops
            .iter()
            .filter_map(node_of)
            .find(|node| matches!(node, Node::KVValueHashFeatureTypeWithChildHash(key, ..) if key == b"ref"))
            .expect("reference row is bound");
        let Node::KVValueHashFeatureTypeWithChildHash(_, _, _, _, child) = ref_row else {
            unreachable!()
        };
        let referenced_bytes = Element::new_item(b"referenced".to_vec())
            .serialize(grove_version)
            .unwrap();
        assert_eq!(
            *child,
            value_hash(&referenced_bytes).unwrap(),
            "a reference row carries the referenced element's value hash"
        );
    }

    /// One composite row, so a mid-generation update cannot be confused with
    /// a race between unrelated parent Merk nodes.
    fn snapshot_fixture(reference_row: bool) -> TempGroveDb {
        let db = make_empty_grovedb();
        let version = GroveVersion::latest();
        db.insert(
            EMPTY_PATH,
            b"ct",
            Element::empty_count_tree(),
            None,
            None,
            version,
        )
        .unwrap()
        .expect("insert host");
        if reference_row {
            db.insert(
                EMPTY_PATH,
                b"target",
                Element::new_item(vec![1]),
                None,
                None,
                version,
            )
            .unwrap()
            .expect("insert reference target");
            db.insert(
                &[b"ct"],
                b"row",
                Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                    b"target".to_vec()
                ])),
                None,
                None,
                version,
            )
            .unwrap()
            .expect("insert reference");
        } else {
            db.insert(&[b"ct"], b"row", Element::empty_tree(), None, None, version)
                .unwrap()
                .expect("insert child tree");
            db.insert(
                &[b"ct".as_slice(), b"row".as_slice()],
                b"item",
                Element::new_item(vec![1]),
                None,
                None,
                version,
            )
            .unwrap()
            .expect("populate child tree");
        }
        db
    }

    /// Commit atomically after the parent ops have been read and before their
    /// composite rows are bound. Both the parent commitment and its second
    /// hash input change together; every committed state is valid.
    fn prove_during_composite_row_commit<T>(
        db: &GroveDb,
        reference_row: bool,
        prove: impl FnOnce() -> T,
    ) -> T {
        use std::{sync::mpsc, time::Duration};

        let (ready_tx, ready_rx) = mpsc::channel();
        let (committed_tx, committed_rx) = mpsc::channel();
        crate::operations::proof::prove_test_hooks::BEFORE_CHUNK_ROW_BINDING.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                ready_tx.send(()).expect("wake writer");
                committed_rx
                    .recv_timeout(Duration::from_secs(20))
                    .expect("writer committed");
            }));
        });
        std::thread::scope(|scope| {
            scope.spawn(move || {
                ready_rx
                    .recv_timeout(Duration::from_secs(20))
                    .expect("parent ops collected");
                let version = GroveVersion::latest();
                let tx = db.start_transaction();
                if reference_row {
                    db.insert(
                        EMPTY_PATH,
                        b"target",
                        Element::new_item(vec![2]),
                        None,
                        Some(&tx),
                        version,
                    )
                    .unwrap()
                    .expect("update reference target");
                    db.insert(
                        &[b"ct"],
                        b"row",
                        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                            b"target".to_vec(),
                        ])),
                        None,
                        Some(&tx),
                        version,
                    )
                    .unwrap()
                    .expect("refresh reference commitment");
                } else {
                    db.insert(
                        &[b"ct".as_slice(), b"row".as_slice()],
                        b"item",
                        Element::new_item(vec![2]),
                        None,
                        Some(&tx),
                        version,
                    )
                    .unwrap()
                    .expect("update child root");
                }
                db.commit_transaction(tx).unwrap().expect("atomic commit");
                committed_tx.send(()).expect("resume generation");
            });
            prove()
        })
    }

    #[test]
    fn trunk_chunk_generation_uses_one_snapshot_across_row_binding_and_ancestors() {
        let version = GroveVersion::latest();
        for reference_row in [false, true] {
            let db = snapshot_fixture(reference_row);
            let root_before = db.root_hash(None, version).unwrap().unwrap();
            let expected_proof = prove_trunk(&db, version);
            let proof =
                prove_during_composite_row_commit(&db, reference_row, || prove_trunk(&db, version));
            assert_ne!(
                db.root_hash(None, version).unwrap().unwrap(),
                root_before,
                "commit landed"
            );
            let (root, result) = GroveDb::verify_trunk_chunk_proof(&proof, &trunk_query(), version)
                .expect("concurrent commit must not invalidate the trunk proof");
            assert_eq!(root, root_before, "ancestors share the generation snapshot");
            assert_eq!(result.elements.len(), 1);
            assert_eq!(proof, expected_proof, "all rows use the pre-commit state");
        }
    }

    #[test]
    fn branch_chunk_generation_uses_one_snapshot_across_row_binding() {
        let version = GroveVersion::latest();
        for reference_row in [false, true] {
            let db = snapshot_fixture(reference_row);
            let query = PathBranchChunkQuery::new(vec![b"ct".to_vec()], b"row".to_vec(), 1);
            let expected = db
                .prove_branch_chunk_non_serialized(&query, version)
                .unwrap()
                .unwrap();
            let expected_proof = encode_ops(&expected.proof);
            let proof = prove_during_composite_row_commit(&db, reference_row, || {
                db.prove_branch_chunk(&query, version)
                    .unwrap()
                    .expect("prove branch")
            });
            let current = db
                .prove_branch_chunk_non_serialized(&query, version)
                .unwrap()
                .unwrap();
            assert_ne!(
                current.branch_root_hash, expected.branch_root_hash,
                "commit landed"
            );
            let result = GroveDb::verify_branch_chunk_proof(
                &proof,
                &query,
                expected.branch_root_hash,
                version,
            )
            .expect("concurrent commit must not invalidate the branch proof");
            assert_eq!(result.elements.len(), 1);
            assert_eq!(proof, expected_proof, "all rows use the pre-commit state");
        }
    }

    // ── Trunk attacks ──────────────────────────────────────────────────

    #[test]
    fn trunk_rejects_downgraded_tree_row_with_forged_metadata() {
        let grove_version = GroveVersion::latest();
        let db = count_tree_with_subtrees(grove_version, Element::empty_count_tree());
        let proof = prove_trunk(&db, grove_version);
        let proof_v1 = decode_v1(&proof);

        let mut ops = target_layer_ops(&proof_v1, b"ct");
        downgrade_first_bound_row(&mut ops, &forged_tree_bytes(grove_version))
            .expect("a bound tree row to downgrade");
        let tampered = rebuild_with_target_ops(proof_v1, b"ct", &ops);

        assert_rejected_unbound(
            GroveDb::verify_trunk_chunk_proof(&tampered, &trunk_query(), grove_version),
            "trunk proof with a downgraded tree row",
        );
    }

    #[test]
    fn trunk_rejects_downgraded_tree_row_in_provable_count_host() {
        let grove_version = GroveVersion::latest();
        let db = count_tree_with_subtrees(grove_version, Element::empty_provable_count_tree());
        let proof = prove_trunk(&db, grove_version);
        let proof_v1 = decode_v1(&proof);

        let mut ops = target_layer_ops(&proof_v1, b"ct");
        // In a Provable* host the merk chunk emits KVValueHashFeatureType,
        // so the downgrade keeps the feature type (and the count it hashes).
        let key = downgrade_first_bound_row(&mut ops, &forged_tree_bytes(grove_version))
            .expect("a bound tree row to downgrade");
        assert!(ops
            .iter()
            .filter_map(node_of)
            .any(|node| matches!(node, Node::KVValueHashFeatureType(k, ..) if *k == key)));
        let tampered = rebuild_with_target_ops(proof_v1, b"ct", &ops);

        assert_rejected_unbound(
            GroveDb::verify_trunk_chunk_proof(&tampered, &trunk_query(), grove_version),
            "trunk proof with a downgraded feature-type tree row",
        );
    }

    #[test]
    fn trunk_rejects_in_place_forged_tree_row() {
        let grove_version = GroveVersion::latest();
        let db = count_tree_with_subtrees(grove_version, Element::empty_count_tree());
        let proof = prove_trunk(&db, grove_version);
        let proof_v1 = decode_v1(&proof);

        let mut ops = target_layer_ops(&proof_v1, b"ct");
        forge_first_bound_row_in_place(&mut ops, &forged_tree_bytes(grove_version))
            .expect("a bound tree row to forge");
        let tampered = rebuild_with_target_ops(proof_v1, b"ct", &ops);

        let result = GroveDb::verify_trunk_chunk_proof(&tampered, &trunk_query(), grove_version);
        let msg = format!("{:?}", result.expect_err("forged bound row rejected"));
        assert!(msg.contains("value/child hash mismatch"), "{msg}");
    }

    #[test]
    fn trunk_rejects_item_disguised_as_tree() {
        let grove_version = GroveVersion::latest();
        let db = count_tree_with_subtrees(grove_version, Element::empty_count_tree());
        let proof = prove_trunk(&db, grove_version);
        let proof_v1 = decode_v1(&proof);

        let mut ops = target_layer_ops(&proof_v1, b"ct");
        disguise_first_item_as_tree(&mut ops, &forged_tree_bytes(grove_version))
            .expect("an item row to disguise");
        let tampered = rebuild_with_target_ops(proof_v1, b"ct", &ops);

        assert_rejected_unbound(
            GroveDb::verify_trunk_chunk_proof(&tampered, &trunk_query(), grove_version),
            "trunk proof with an item disguised as a tree",
        );
    }

    // ── Branch ─────────────────────────────────────────────────────────

    /// Count tree of 100 hashed keys mixing items, empty subtrees and
    /// references, so a trunk at depth 5 leaves branches holding composite
    /// rows.
    fn wide_count_tree(grove_version: &GroveVersion) -> TempGroveDb {
        let db = make_empty_grovedb();
        db.insert(
            EMPTY_PATH,
            b"ct",
            Element::empty_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert count tree");
        db.insert(
            EMPTY_PATH,
            b"target",
            Element::new_item(b"referenced".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert target");
        for i in 0u32..100 {
            let mut hasher = Hasher::new();
            hasher.update(&i.to_be_bytes());
            let key: [u8; 32] = *hasher.finalize().as_bytes();
            let element = match i % 3 {
                0 => Element::new_item(vec![i as u8; 4]),
                1 => Element::empty_tree(),
                _ => Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                    b"target".to_vec(),
                ])),
            };
            db.insert(&[b"ct"], &key, element, None, None, grove_version)
                .unwrap()
                .expect("insert row");
        }
        db
    }

    /// Every branch of the wide tree, as `(query, honest proof, expected
    /// hash)`.
    fn wide_tree_branches(
        db: &TempGroveDb,
        grove_version: &GroveVersion,
    ) -> Vec<(PathBranchChunkQuery, Vec<u8>, [u8; 32])> {
        let trunk_query = PathTrunkChunkQuery::new(vec![b"ct".to_vec()], 5);
        let trunk_proof = db
            .prove_trunk_chunk(&trunk_query, grove_version)
            .unwrap()
            .expect("prove trunk");
        let (_, trunk) =
            GroveDb::verify_trunk_chunk_proof(&trunk_proof, &trunk_query, grove_version)
                .expect("honest trunk verifies");
        let remaining_depth = trunk.chunk_depths[1];
        trunk
            .leaf_keys
            .iter()
            .map(|(leaf_key, leaf_info)| {
                let query = PathBranchChunkQuery::new(
                    vec![b"ct".to_vec()],
                    leaf_key.clone(),
                    remaining_depth,
                );
                let proof = db
                    .prove_branch_chunk(&query, grove_version)
                    .unwrap()
                    .expect("prove branch");
                (query, proof, leaf_info.hash)
            })
            .collect()
    }

    #[test]
    fn branch_v4_binds_every_composite_row_and_honest_branches_verify() {
        let grove_version = GroveVersion::latest();
        let db = wide_count_tree(grove_version);

        let mut trees = 0;
        let mut references = 0;
        for (query, proof, expected_hash) in wide_tree_branches(&db, grove_version) {
            let result =
                GroveDb::verify_branch_chunk_proof(&proof, &query, expected_hash, grove_version)
                    .expect("honest branch verifies");
            for element in result.elements.values() {
                if element.is_any_tree() {
                    trees += 1;
                } else if element.is_reference() {
                    references += 1;
                }
            }
            let ops: Vec<Op> = Decoder::new(&proof).collect::<Result<_, _>>().unwrap();
            for node in ops.iter().filter_map(node_of) {
                if let Node::KVValueHash(key, ..) | Node::KVValueHashFeatureType(key, ..) = node {
                    panic!(
                        "branch row {} left unbound under GROVE_V4",
                        hex::encode(key)
                    );
                }
            }
        }
        assert!(
            trees > 0 && references > 0,
            "branches should hold composite rows"
        );
    }

    #[test]
    fn branch_rejects_downgraded_tree_row_with_forged_metadata() {
        let grove_version = GroveVersion::latest();
        let db = wide_count_tree(grove_version);
        let forged = forged_tree_bytes(grove_version);

        let mut attacked = 0;
        for (query, proof, expected_hash) in wide_tree_branches(&db, grove_version) {
            let mut ops: Vec<Op> = Decoder::new(&proof).collect::<Result<_, _>>().unwrap();
            if downgrade_first_bound_row(&mut ops, &forged).is_none() {
                continue;
            }
            attacked += 1;
            assert_rejected_unbound(
                GroveDb::verify_branch_chunk_proof(
                    &encode_ops(&ops),
                    &query,
                    expected_hash,
                    grove_version,
                ),
                "branch proof with a downgraded composite row",
            );
        }
        assert!(attacked > 0, "no branch held a bound composite row");
    }

    #[test]
    fn branch_rejects_item_disguised_as_tree() {
        let grove_version = GroveVersion::latest();
        let db = wide_count_tree(grove_version);
        let forged = forged_tree_bytes(grove_version);

        let mut attacked = 0;
        for (query, proof, expected_hash) in wide_tree_branches(&db, grove_version) {
            let mut ops: Vec<Op> = Decoder::new(&proof).collect::<Result<_, _>>().unwrap();
            if disguise_first_item_as_tree(&mut ops, &forged).is_none() {
                continue;
            }
            attacked += 1;
            assert_rejected_unbound(
                GroveDb::verify_branch_chunk_proof(
                    &encode_ops(&ops),
                    &query,
                    expected_hash,
                    grove_version,
                ),
                "branch proof with an item disguised as a tree",
            );
        }
        assert!(attacked > 0, "no branch held an item row");
    }

    // ── Non-Merk trees ─────────────────────────────────────────────────

    #[test]
    fn trunk_binds_non_merk_tree_rows_and_rejects_forged_entry_count() {
        let grove_version = GroveVersion::latest();
        let db = count_tree_with_subtrees(grove_version, Element::empty_count_tree());
        db.insert(
            &[b"ct"],
            b"mmr",
            Element::empty_mmr_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert mmr");
        for i in 0u8..2 {
            db.mmr_tree_append(
                [b"ct".as_slice()].as_ref(),
                b"mmr",
                vec![i; 8],
                None,
                grove_version,
            )
            .unwrap()
            .expect("append leaf");
        }

        let proof = prove_trunk(&db, grove_version);
        let (_, result) = GroveDb::verify_trunk_chunk_proof(&proof, &trunk_query(), grove_version)
            .expect("honest proof verifies");
        let stored = db
            .get_raw(
                grovedb_path::SubtreePath::from([b"ct".as_slice()].as_ref()),
                b"mmr",
                None,
                grove_version,
            )
            .unwrap()
            .expect("stored mmr element");
        assert!(matches!(stored, Element::MmrTree(size, _) if size > 0));
        assert_eq!(
            result.elements.get(b"mmr".as_slice()),
            Some(&stored),
            "returned MMR must be the stored element"
        );

        let proof_v1 = decode_v1(&proof);
        let mut ops = target_layer_ops(&proof_v1, b"ct");
        let forged_mmr = Element::new_mmr_tree(999, None)
            .serialize(grove_version)
            .unwrap();
        let mut found = false;
        for op in ops.iter_mut() {
            if let Op::Push(Node::KVValueHashFeatureTypeWithChildHash(key, _, vh, ..)) = op
                && key == b"mmr"
            {
                *op = Op::Push(Node::KVValueHash(key.clone(), forged_mmr.clone(), *vh));
                found = true;
            }
        }
        assert!(found, "the MMR row is bound and within the trunk");
        let tampered = rebuild_with_target_ops(proof_v1, b"ct", &ops);
        assert_rejected_unbound(
            GroveDb::verify_trunk_chunk_proof(&tampered, &trunk_query(), grove_version),
            "trunk proof with a forged MMR entry count",
        );
    }

    // ── Indexed trees ──────────────────────────────────────────────────

    #[test]
    fn chunk_proofs_refuse_indexed_tree_rows() {
        let grove_version = GroveVersion::latest();
        let db = count_tree_with_subtrees(grove_version, Element::empty_count_tree());
        db.insert(
            &[b"ct"],
            b"idx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert indexed tree");

        let err = db
            .prove_trunk_chunk(&trunk_query(), grove_version)
            .unwrap()
            .expect_err("trunk over a host with an indexed row is refused");
        assert!(
            matches!(&err, Error::NotSupported(msg) if msg.contains("indexed tree")),
            "unexpected error: {err:?}"
        );

        let branch_query = PathBranchChunkQuery::new(vec![b"ct".to_vec()], b"idx".to_vec(), 1);
        let err = db
            .prove_branch_chunk(&branch_query, grove_version)
            .unwrap()
            .expect_err("branch rooted at an indexed row is refused");
        assert!(
            matches!(&err, Error::NotSupported(msg) if msg.contains("indexed tree")),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn check_chunk_proof_row_v1_rejects_rows_no_node_form_can_bind() {
        let grove_version = GroveVersion::latest();
        let key = b"k".to_vec();

        // An indexed tree's three-input binding has no carrier: rejected in
        // every node form, even one whose two-input composition checks out.
        let indexed = Element::empty_provable_count_indexed_tree();
        let indexed_bytes = indexed.serialize(grove_version).unwrap();
        let child = [9u8; 32];
        let vh = combine_hash(&value_hash(&indexed_bytes).unwrap(), &child).unwrap();
        let node = Node::KVValueHashFeatureTypeWithChildHash(
            key.clone(),
            indexed_bytes.clone(),
            vh,
            TreeFeatureType::BasicMerkNode,
            child,
        );
        let msg = format!(
            "{:?}",
            GroveDb::check_chunk_proof_row(&node, &key, &indexed_bytes, &indexed, grove_version)
                .expect_err("indexed row rejected")
        );
        assert!(msg.contains("indexed tree"), "{msg}");

        // A tree in a plain KV node: never committed that way.
        let tree = Element::empty_tree();
        let tree_bytes = tree.serialize(grove_version).unwrap();
        let node = Node::KV(key.clone(), tree_bytes.clone());
        let msg = format!(
            "{:?}",
            GroveDb::check_chunk_proof_row(&node, &key, &tree_bytes, &tree, grove_version)
                .expect_err("tree in KV rejected")
        );
        assert!(msg.contains("must carry its child hash"), "{msg}");

        // An item in a child-hash node whose composition checks out: the
        // row cannot have that shape.
        let item = Element::new_item(vec![1]);
        let item_bytes = item.serialize(grove_version).unwrap();
        let vh = combine_hash(&value_hash(&item_bytes).unwrap(), &child).unwrap();
        let node = Node::KVValueHashFeatureTypeWithChildHash(
            key.clone(),
            item_bytes.clone(),
            vh,
            TreeFeatureType::BasicMerkNode,
            child,
        );
        let msg = format!(
            "{:?}",
            GroveDb::check_chunk_proof_row(&node, &key, &item_bytes, &item, grove_version)
                .expect_err("item in child-hash node rejected")
        );
        assert!(msg.contains("neither a tree nor a reference"), "{msg}");

        // The honest shapes pass.
        let node = Node::KV(key.clone(), item_bytes.clone());
        GroveDb::check_chunk_proof_row(&node, &key, &item_bytes, &item, grove_version)
            .expect("item in KV accepted");
        let vh = combine_hash(&value_hash(&tree_bytes).unwrap(), &child).unwrap();
        let node = Node::KVValueHashFeatureTypeWithChildHash(
            key.clone(),
            tree_bytes.clone(),
            vh,
            TreeFeatureType::BasicMerkNode,
            child,
        );
        GroveDb::check_chunk_proof_row(&node, &key, &tree_bytes, &tree, grove_version)
            .expect("bound tree accepted");
    }

    /// The released (v0) row check, pinned arm by arm: the tree waiver, the
    /// item mismatch rejection, the child-hash composition check, and the
    /// `KVRefValueHash` rejection.
    #[test]
    fn check_chunk_proof_row_v0_pins_released_arms() {
        let grove_version = &GROVE_V3;
        let key = b"k".to_vec();
        let child = [9u8; 32];

        let tree = Element::empty_tree();
        let tree_bytes = tree.serialize(grove_version).unwrap();
        let item = Element::new_item(vec![1]);
        let item_bytes = item.serialize(grove_version).unwrap();

        // A tree in a bare KVValueHash whose value_hash is a combine_hash the
        // bytes cannot reproduce: waived (the hole this PR closes from V4).
        let combined = combine_hash(&value_hash(&tree_bytes).unwrap(), &child).unwrap();
        let node = Node::KVValueHash(key.clone(), tree_bytes.clone(), combined);
        GroveDb::check_chunk_proof_row(&node, &key, &tree_bytes, &tree, grove_version)
            .expect("v0 waives the mismatch for a tree");
        let node = Node::KVValueHashFeatureType(
            key.clone(),
            tree_bytes.clone(),
            combined,
            TreeFeatureType::BasicMerkNode,
        );
        GroveDb::check_chunk_proof_row(&node, &key, &tree_bytes, &tree, grove_version)
            .expect("v0 waives the mismatch for a tree in a feature-type node");

        // An item with a mismatching value_hash is rejected.
        let node = Node::KVValueHash(key.clone(), item_bytes.clone(), combined);
        let msg = format!(
            "{:?}",
            GroveDb::check_chunk_proof_row(&node, &key, &item_bytes, &item, grove_version)
                .expect_err("v0 rejects an item mismatch")
        );
        assert!(msg.contains("value hash mismatch"), "{msg}");
        // ... and accepted when the hash matches.
        let node = Node::KVValueHash(
            key.clone(),
            item_bytes.clone(),
            value_hash(&item_bytes).unwrap(),
        );
        GroveDb::check_chunk_proof_row(&node, &key, &item_bytes, &item, grove_version)
            .expect("v0 accepts a matching item");

        // Child-hash node: composition checked.
        let node = Node::KVValueHashFeatureTypeWithChildHash(
            key.clone(),
            tree_bytes.clone(),
            combined,
            TreeFeatureType::BasicMerkNode,
            child,
        );
        GroveDb::check_chunk_proof_row(&node, &key, &tree_bytes, &tree, grove_version)
            .expect("v0 accepts a correct child-hash composition");
        let node = Node::KVValueHashFeatureTypeWithChildHash(
            key.clone(),
            tree_bytes.clone(),
            combined,
            TreeFeatureType::BasicMerkNode,
            [0u8; 32],
        );
        let msg = format!(
            "{:?}",
            GroveDb::check_chunk_proof_row(&node, &key, &tree_bytes, &tree, grove_version)
                .expect_err("v0 rejects a wrong child hash")
        );
        assert!(msg.contains("value/child hash mismatch"), "{msg}");

        // KVRefValueHash family: never accepted.
        let node = Node::KVRefValueHash(key.clone(), item_bytes.clone(), combined);
        let msg = format!(
            "{:?}",
            GroveDb::check_chunk_proof_row(&node, &key, &item_bytes, &item, grove_version)
                .expect_err("v0 rejects KVRefValueHash")
        );
        assert!(msg.contains("unexpected KVRefValueHash"), "{msg}");

        // Plain KV: bound by the merk verifier itself.
        let node = Node::KV(key.clone(), item_bytes.clone());
        GroveDb::check_chunk_proof_row(&node, &key, &item_bytes, &item, grove_version)
            .expect("v0 accepts KV");
    }

    #[test]
    fn check_chunk_proof_row_v1_rejects_ref_nodes_and_item_mismatch() {
        let grove_version = GroveVersion::latest();
        let key = b"k".to_vec();
        let item = Element::new_item(vec![1]);
        let item_bytes = item.serialize(grove_version).unwrap();
        let real_vh = value_hash(&item_bytes).unwrap();

        let node = Node::KVRefValueHashSum(key.clone(), item_bytes.clone(), real_vh, 1);
        let msg = format!(
            "{:?}",
            GroveDb::check_chunk_proof_row(&node, &key, &item_bytes, &item, grove_version)
                .expect_err("v1 rejects KVRefValueHash family")
        );
        assert!(msg.contains("unexpected KVRefValueHash"), "{msg}");

        let node = Node::KVValueHash(key.clone(), item_bytes.clone(), [7u8; 32]);
        let msg = format!(
            "{:?}",
            GroveDb::check_chunk_proof_row(&node, &key, &item_bytes, &item, grove_version)
                .expect_err("v1 rejects an item mismatch")
        );
        assert!(msg.contains("value hash mismatch"), "{msg}");

        let node = Node::KVValueHash(key.clone(), item_bytes.clone(), real_vh);
        GroveDb::check_chunk_proof_row(&node, &key, &item_bytes, &item, grove_version)
            .expect("v1 accepts a matching item in KVValueHash");
    }

    #[test]
    fn chunk_proof_row_binding_unknown_version_is_refused_on_both_sides() {
        let mut unknown = GroveVersion::latest().clone();
        unknown
            .grovedb_versions
            .operations
            .proof
            .chunk_proof_row_binding = 99;

        let key = b"k".to_vec();
        let item = Element::new_item(vec![1]);
        let item_bytes = item.serialize(&unknown).unwrap();
        let node = Node::KV(key.clone(), item_bytes.clone());
        assert!(matches!(
            GroveDb::check_chunk_proof_row(&node, &key, &item_bytes, &item, &unknown),
            Err(Error::VersionError(_))
        ));

        let db = count_tree_with_subtrees(GroveVersion::latest(), Element::empty_count_tree());
        assert!(matches!(
            db.prove_trunk_chunk(&trunk_query(), &unknown).unwrap(),
            Err(Error::VersionError(_))
        ));
        let branch_query = PathBranchChunkQuery::new(vec![b"ct".to_vec()], b"sub_0".to_vec(), 1);
        assert!(matches!(
            db.prove_branch_chunk(&branch_query, &unknown).unwrap(),
            Err(Error::VersionError(_))
        ));
    }

    // ── Version gate ───────────────────────────────────────────────────

    /// Consensus version gate for `proof.chunk_proof_row_binding`.
    ///
    /// Under **v0** (`GROVE_V3`, the released shape) the trunk prover emits
    /// a bare `KVValueHash` for a subtree row and the verifier waives the
    /// value-hash check for it, so forged tree metadata verifies against the
    /// real root hash. Under **v1** (`GROVE_V4`+) the row carries its child
    /// hash, the verifier requires it, and the same forgery is rejected.
    ///
    /// Pins both sides so the gate cannot silently collapse to one behaviour
    /// — which would either reject honest released proofs or reopen the
    /// forgery.
    #[test]
    fn chunk_proof_row_binding_version_gate() {
        let build_and_forge = |grove_version: &GroveVersion| {
            let db = count_tree_with_subtrees(grove_version, Element::empty_count_tree());
            let proof = prove_trunk(&db, grove_version);
            GroveDb::verify_trunk_chunk_proof(&proof, &trunk_query(), grove_version)
                .expect("honest proof verifies at every version");

            let proof_v1 = decode_v1(&proof);
            let mut ops = target_layer_ops(&proof_v1, b"ct");
            let bound = ops
                .iter()
                .filter_map(node_of)
                .any(|node| matches!(node, Node::KVValueHashFeatureTypeWithChildHash(..)));
            let forged = forged_tree_bytes(grove_version);
            let forged_key = if bound {
                downgrade_first_bound_row(&mut ops, &forged)
            } else {
                let mut key = None;
                for op in ops.iter_mut() {
                    if let Op::Push(Node::KVValueHash(k, value, _)) = op
                        && key.is_none()
                    {
                        key = Some(k.clone());
                        *value = forged.clone();
                    }
                }
                key
            }
            .expect("a subtree row to forge");
            let tampered = rebuild_with_target_ops(proof_v1, b"ct", &ops);
            let accepted =
                match GroveDb::verify_trunk_chunk_proof(&tampered, &trunk_query(), grove_version) {
                    Ok((_, result)) => {
                        assert!(
                            matches!(
                                result.elements.get(&forged_key),
                                Some(Element::CountTree(Some(_), 1_000_000, _))
                            ),
                            "an accepted forgery returns the forged metadata"
                        );
                        true
                    }
                    Err(_) => false,
                };
            (bound, accepted)
        };

        // v0 — GROVE_V3, the released shape. Bare KVValueHash, forgery
        // accepted. This is the hole; it cannot be closed in place because
        // V3 is live.
        let (v3_bound, v3_accepted) = build_and_forge(&GROVE_V3);
        assert!(
            !v3_bound,
            "GROVE_V3 must keep emitting a bare KVValueHash for a subtree row"
        );
        assert!(
            v3_accepted,
            "GROVE_V3 is expected to still accept the forgery — if this now fails, the fix \
             leaked into a released version and changes consensus behaviour"
        );

        // v1 — GROVE_V4, latest. Child-hash node, forgery rejected.
        let (v4_bound, v4_accepted) = build_and_forge(GroveVersion::latest());
        assert!(
            v4_bound,
            "GROVE_V4 must emit KVValueHashFeatureTypeWithChildHash for a subtree row"
        );
        assert!(!v4_accepted, "GROVE_V4 must reject forged tree metadata");
    }

    /// A V0 trunk envelope (produced under `GROVE_V1`) carries unbound tree
    /// rows. It still verifies under the version that produced it, and a
    /// `GROVE_V4` verifier rejects it rather than return unbound metadata.
    #[test]
    fn v0_trunk_envelope_tree_rows_are_rejected_under_v4() {
        let db = count_tree_with_subtrees(&GROVE_V1, Element::empty_count_tree());
        let proof = prove_trunk(&db, &GROVE_V1);
        assert!(
            matches!(
                bincode::decode_from_slice::<GroveDBProof, _>(&proof, bincode_config())
                    .unwrap()
                    .0,
                GroveDBProof::V0(_)
            ),
            "GROVE_V1 produces a V0 trunk envelope"
        );

        GroveDb::verify_trunk_chunk_proof(&proof, &trunk_query(), &GROVE_V1)
            .expect("V0 envelope verifies under GROVE_V1");
        let msg = format!(
            "{:?}",
            GroveDb::verify_trunk_chunk_proof(&proof, &trunk_query(), GroveVersion::latest())
                .expect_err("V0 envelope with unbound tree rows is rejected under GROVE_V4")
        );
        assert!(msg.contains("must carry its child hash"), "{msg}");
    }
}
