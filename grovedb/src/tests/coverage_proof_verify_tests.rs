//! Coverage tests targeting uncovered diff lines of PR #657.

#[cfg(test)]
mod tests {
    use std::collections::LinkedList;

    use grovedb_element::indexed::IndexAxis;
    use grovedb_merk::{
        proofs::{encode_into, Decoder, Node, Op},
        tree::{axes_digest, combine_hash_three, value_hash, TreeFeatureType, NULL_HASH},
        CryptoHash,
    };
    use grovedb_version::version::GroveVersion;

    use crate::{
        operations::proof::{
            GroveDBProof, GroveDBProofV0, GroveDBProofV1, LayerProof, MerkOnlyLayerProof,
            ProofBytes, ProveOptions,
        },
        query_result_type::{PathKeyElementTrio, PathKeyOptionalElementTrio, QueryResultType},
        tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
        Element, Error, GroveDb, PathQuery, Query, SizedQuery,
    };

    // =================================================================
    // Shared helpers
    // =================================================================

    /// The exact bincode configuration `decode_grovedb_proof_canonical`
    /// uses. Re-encoding a tampered envelope with anything else would be
    /// rejected as malformed before reaching the branch under test.
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

    /// `[TEST_LEAF]`-rooted query selecting a single key with no
    /// subquery — the shape that makes the queried element itself the
    /// terminal result.
    fn key_query(key: &[u8]) -> PathQuery {
        let mut q = Query::new();
        q.insert_key(key.to_vec());
        PathQuery::new(vec![TEST_LEAF.to_vec()], SizedQuery::new(q, None, None))
    }

    /// `[TEST_LEAF]`-rooted query selecting a single key and descending
    /// into everything below it.
    fn key_subquery(key: &[u8], limit: Option<u16>, add_parent_tree: bool) -> PathQuery {
        let mut inner = Query::new();
        inner.insert_all();
        let mut q = Query::new();
        q.insert_key(key.to_vec());
        q.set_subquery(inner);
        q.add_parent_tree_on_subquery = add_parent_tree;
        PathQuery::new(vec![TEST_LEAF.to_vec()], SizedQuery::new(q, limit, None))
    }

    fn query_raw_trios(
        db: &TempGroveDb,
        path_query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> Vec<PathKeyElementTrio> {
        let (elements, _) = db
            .query_raw(
                path_query,
                true,
                true,
                true,
                QueryResultType::QueryPathKeyElementTrioResultType,
                None,
                grove_version,
            )
            .unwrap()
            .expect("query_raw should succeed");
        elements.to_path_key_elements()
    }

    /// Assert an honest proof verifies, binds to the live root hash, and
    /// returns exactly what `query_raw` returns.
    fn assert_verifies_like_query_raw(
        db: &TempGroveDb,
        proof: &[u8],
        path_query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> Vec<PathKeyOptionalElementTrio> {
        let (root, items) =
            GroveDb::verify_query(proof, path_query, grove_version).expect("honest proof verifies");
        assert_eq!(
            root,
            db.root_hash(None, grove_version).unwrap().expect("root"),
            "verified root must match the live root hash"
        );
        let raw = query_raw_trios(db, path_query, grove_version);
        let verified: Vec<PathKeyElementTrio> = items
            .iter()
            .map(|(path, key, element)| {
                (
                    path.clone(),
                    key.clone(),
                    element.clone().expect("verified element present"),
                )
            })
            .collect();
        assert_eq!(
            verified, raw,
            "verified result set must equal the query_raw result set"
        );
        items
    }

    /// Rewrite the value bytes of the value-bearing proof node for
    /// `target_key` inside a layer's Merk proof, leaving the committed
    /// `value_hash` untouched. A `KVValueHash*` node commits only to
    /// (key, value_hash), so the Merk-level chain still reconstructs —
    /// which is exactly why the GroveDB layer has to re-derive the
    /// expected value hash from the element bytes itself.
    fn forge_value_bytes_in_layer(layer: &mut LayerProof, target_key: &[u8], new_value: Vec<u8>) {
        let original = match &layer.merk_proof {
            ProofBytes::Merk(b) => b.clone(),
            _ => panic!("layer under forge must carry ProofBytes::Merk"),
        };
        let mut ops: LinkedList<Op> = LinkedList::new();
        let mut replaced = false;
        let mut seen = Vec::new();
        for op in Decoder::new(&original) {
            let op = op.expect("decode proof op");
            let new_op = match op {
                Op::Push(Node::KVValueHashFeatureType(k, v, vh, ft)) => {
                    seen.push(hex::encode(&k));
                    if k.as_slice() == target_key {
                        replaced = true;
                        Op::Push(Node::KVValueHashFeatureType(k, new_value.clone(), vh, ft))
                    } else {
                        Op::Push(Node::KVValueHashFeatureType(k, v, vh, ft))
                    }
                }
                Op::Push(Node::KVValueHash(k, v, vh)) => {
                    seen.push(hex::encode(&k));
                    if k.as_slice() == target_key {
                        replaced = true;
                        Op::Push(Node::KVValueHash(k, new_value.clone(), vh))
                    } else {
                        Op::Push(Node::KVValueHash(k, v, vh))
                    }
                }
                Op::Push(Node::KVValueHashFeatureTypeWithChildHash(k, v, vh, ft, ch)) => {
                    seen.push(hex::encode(&k));
                    if k.as_slice() == target_key {
                        replaced = true;
                        Op::Push(Node::KVValueHashFeatureTypeWithChildHash(
                            k,
                            new_value.clone(),
                            vh,
                            ft,
                            ch,
                        ))
                    } else {
                        Op::Push(Node::KVValueHashFeatureTypeWithChildHash(k, v, vh, ft, ch))
                    }
                }
                other => other,
            };
            ops.push_back(new_op);
        }
        assert!(
            replaced,
            "forge: key {} not found as a value-bearing proof node (saw {:?})",
            hex::encode(target_key),
            seen
        );
        let mut out = Vec::with_capacity(original.len() + new_value.len());
        encode_into(ops.iter(), &mut out);
        layer.merk_proof = ProofBytes::Merk(out);
    }

    // =================================================================
    // 1. count-offset leaf dispatch: NonCounted rejection (verify.rs
    //    550-558) and the empty-PCPSIT axes-digest expectation
    //    (verify.rs 614-620).
    // =================================================================

    /// `ProvableCountTree` at the root holding 15 single-letter Item
    /// keys, plus the offset-paginated path query used by the forge
    /// tests (offset 5, limit 3 → {"f","g","h"}).
    fn count_offset_fixture(grove_version: &GroveVersion) -> (TempGroveDb, Vec<u8>, PathQuery) {
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
        .expect("insert ProvableCountTree");
        for i in 0..15u8 {
            let key = vec![b'a' + i];
            db.insert(
                &[b"counts"],
                key.as_slice(),
                Element::new_item(format!("v_{}", i).into_bytes()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert item");
        }
        let mut q = Query::new();
        q.insert_range_inclusive(b"a".to_vec()..=b"o".to_vec());
        let path_query = PathQuery::new(
            vec![b"counts".to_vec()],
            SizedQuery::new(q, Some(3), Some(5)),
        );
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove honest count-offset page");
        (db, proof, path_query)
    }

    /// Swap the honest `KVCount(key, value, count)` leaf node for a
    /// `KVValueHashFeatureType` carrying `forged_value` and the original
    /// value hash. The Merk-level count check still passes (the feature
    /// type keeps the honest count) and the chain hash is unchanged, so
    /// the forged bytes reach the GroveDB-level defense-in-depth checks
    /// in `run_count_offset_layer_dispatch`.
    fn forge_count_offset_value(
        honest_proof: Vec<u8>,
        target_key: &[u8],
        forged_value: Vec<u8>,
    ) -> Vec<u8> {
        let mut root_layer = decode_v1_root(&honest_proof);
        let leaf = root_layer
            .lower_layers
            .get_mut(b"counts".as_slice())
            .expect("leaf layer at counts");
        let original = match &leaf.merk_proof {
            ProofBytes::Merk(b) => b.clone(),
            _ => panic!("leaf merk_proof must be ProofBytes::Merk"),
        };
        let mut ops: LinkedList<Op> = LinkedList::new();
        let mut replaced = false;
        for op in Decoder::new(&original) {
            let op = op.expect("decode proof op");
            let new_op = match op {
                Op::Push(Node::KVCount(k, v, count)) if k.as_slice() == target_key => {
                    replaced = true;
                    Op::Push(Node::KVValueHashFeatureType(
                        k,
                        forged_value.clone(),
                        value_hash(&v).unwrap(),
                        TreeFeatureType::ProvableCountedMerkNode(count),
                    ))
                }
                other => other,
            };
            ops.push_back(new_op);
        }
        assert!(
            replaced,
            "forge: {} not found as a KVCount node in the honest proof",
            hex::encode(target_key)
        );
        let mut out = Vec::with_capacity(original.len() + forged_value.len());
        encode_into(ops.iter(), &mut out);
        leaf.merk_proof = ProofBytes::Merk(out);
        encode_v1_root(root_layer)
    }

    /// verify.rs 550-558: a count-offset page that surfaces a
    /// `NonCounted`-wrapped entry must be rejected as a forgery.
    ///
    /// The Merk-level KV→KVValueHash guard only catches wrappers whose
    /// base element has a *simple* value hash (Item / SumItem /
    /// ItemWithSumItem). `NonCounted(Tree)` has a combined value hash,
    /// so it slips past that guard and has to be caught by the GroveDB
    /// blacklist — the honest prover never emits NonCounted entries in
    /// `returned_items` because they carry `own_count = 0`.
    #[test]
    fn count_offset_verifier_rejects_forged_non_counted_tree_return() {
        let v = GroveVersion::latest();
        let (_db, honest, path_query) = count_offset_fixture(v);
        let forged = Element::new_non_counted(Element::empty_tree())
            .expect("wrap tree in NonCounted")
            .serialize(v)
            .expect("serialize NonCounted(Tree)");
        let tampered = forge_count_offset_value(honest, b"f", forged);

        let err = GroveDb::verify_query_raw(&tampered, &path_query, v)
            .expect_err("a NonCounted-wrapped count-offset return must be rejected");
        assert!(
            matches!(
                err,
                Error::InvalidProof(_, ref msg)
                    if msg.contains("do not surface")
                        && msg.contains("NonCounted-wrapped entries in returned items")
                        && msg.contains("appears forged")
            ),
            "expected the GroveDB-level NonCounted blacklist rejection, got {err:?}"
        );
    }

    /// verify.rs 614-620: an empty `ProvableCountProvableSumIndexedTree`
    /// returned by a count-offset page must have its expected value hash
    /// computed as `combine_hash_three(H(value), NULL_HASH,
    /// axes_digest(zero_axes))`, not the two-input `combine_hash` used
    /// for every other empty tree.
    ///
    /// An empty PCPSIT is not honest-reachable through this path (PCIT /
    /// PCPSIT children of a `ProvableCountTree` are NonCounted-wrapped
    /// and rejected one branch earlier — see
    /// `count_offset_accepts_empty_psit_return`, which pins the PSIT
    /// case honestly), so the arm is exercised with a forged return. The
    /// assertion pins the *expected* hash printed in the rejection
    /// message to the three-input axes-digest form: reverting the arm to
    /// the two-input `combine_hash` fallback changes that hash and fails
    /// this test.
    #[test]
    fn count_offset_empty_pcpsit_return_expects_axes_digest_value_hash() {
        let v = GroveVersion::latest();
        let (_db, honest, path_query) = count_offset_fixture(v);

        let axes: Vec<(u8, Option<Vec<u8>>)> = vec![
            (IndexAxis::Count.tag(), None),
            (IndexAxis::Sum.tag(), None),
            (IndexAxis::Avg.tag(), None),
        ];
        let forged_element = Element::empty_provable_count_provable_sum_indexed_tree(axes.clone())
            .expect("canonical axes");
        let forged_bytes = forged_element.serialize(v).expect("serialize empty PCPSIT");

        let zero_axes: Vec<(u8, CryptoHash)> = axes.iter().map(|(t, _)| (*t, NULL_HASH)).collect();
        let digest = axes_digest(&zero_axes).unwrap();
        let expected =
            combine_hash_three(&value_hash(&forged_bytes).unwrap(), &NULL_HASH, &digest).unwrap();

        let tampered = forge_count_offset_value(honest, b"f", forged_bytes);
        let err = GroveDb::verify_query_raw(&tampered, &path_query, v)
            .expect_err("a forged empty-PCPSIT return must be rejected");
        match err {
            Error::InvalidProof(_, ref msg) => {
                assert!(
                    msg.contains("empty-tree return at key")
                        && msg.contains("KV→KVValueHash forgery"),
                    "expected the count-offset empty-tree value-hash rejection, got {msg}"
                );
                assert!(
                    msg.contains(&hex::encode(expected)),
                    "the expected hash must be the three-input axes-digest form {} — the \
                     PCPSIT arm did not run; message was {msg}",
                    hex::encode(expected)
                );
            }
            other => panic!("expected InvalidProof, got {other:?}"),
        }
    }

    // =================================================================
    // 2. V1 indexed-tree terminal attestation rejections
    //    (verify.rs 814-844).
    // =================================================================

    /// `[TEST_LEAF]/cidx` = populated PCIT. The returned proof answers
    /// `key_query(b"cidx")`, i.e. the indexed tree is itself the result
    /// with nothing queried below it, so its lower layer is the
    /// `ProofBytes::IndexedTreeTerminal` attestation.
    fn pcit_terminal_fixture(grove_version: &GroveVersion) -> (TempGroveDb, Vec<u8>, PathQuery) {
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert PCIT");
        for k in [b"a".as_ref(), b"b", b"c"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::new_item(b"v".to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("populate PCIT");
        }
        let path_query = key_query(b"cidx");
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove terminal PCIT");
        (db, proof, path_query)
    }

    /// Borrow the `[TEST_LEAF] → key` lower layer of a decoded V1 proof.
    fn lower_layer_at<'a>(root: &'a mut LayerProof, key: &[u8]) -> &'a mut LayerProof {
        root.lower_layers
            .get_mut(TEST_LEAF)
            .expect("TEST_LEAF lower layer present")
            .lower_layers
            .get_mut(key)
            .expect("keyed lower layer present")
    }

    /// The honest fixture really does carry a 64-byte terminal
    /// attestation, and it verifies. Guards the three rejection tests
    /// below from silently testing the wrong proof shape.
    ///
    /// NOTE: the terminal-attestation arm pushes the indexed key onto
    /// `path` *before* building the result, so the verified entry is
    /// reported as `(path = [TEST_LEAF, cidx], key = cidx)` whereas
    /// `query_raw` — and the regular-tree terminal arm of this same
    /// verifier, which builds its result from `current_path` — report
    /// `(path = [TEST_LEAF], key = cidx)`. The key and element agree;
    /// only the path differs. This test pins the current behavior
    /// rather than `assert_verifies_like_query_raw` so the divergence
    /// is visible instead of silently baked in.
    #[test]
    fn v1_indexed_terminal_fixture_is_honest_and_verifies() {
        let v = GroveVersion::latest();
        let (db, proof, path_query) = pcit_terminal_fixture(v);
        let (root, items) =
            GroveDb::verify_query(&proof, &path_query, v).expect("honest proof verifies");
        assert_eq!(
            root,
            db.root_hash(None, v).unwrap().expect("root"),
            "verified root must match the live root hash"
        );
        assert_eq!(items.len(), 1, "the PCIT element is the single result");

        let raw = query_raw_trios(&db, &path_query, v);
        assert_eq!(raw.len(), 1, "query_raw returns the same single element");
        assert_eq!(items[0].1, raw[0].1, "keys must agree with query_raw");
        assert_eq!(
            items[0].2.as_ref(),
            Some(&raw[0].2),
            "elements must agree with query_raw"
        );
        assert_eq!(
            items[0].0, raw[0].0,
            "the indexed terminal arm must report the result under the PARENT path, \
             exactly like query_raw and the regular-tree terminal arm — including the \
             element's own key made (path, key) lookups miss, so absence-proof \
             verification reported an existing indexed tree as absent"
        );

        let mut root = decode_v1_root(&proof);
        let terminal = lower_layer_at(&mut root, b"cidx");
        match &terminal.merk_proof {
            ProofBytes::IndexedTreeTerminal(bytes) => assert_eq!(
                bytes.len(),
                64,
                "terminal attestation is attestation || primary_root"
            ),
            _ => panic!("expected an IndexedTreeTerminal lower layer"),
        }
        assert!(
            terminal.lower_layers.is_empty(),
            "an honest terminal attestation carries no further layers"
        );
    }

    /// verify.rs 814-822: only the terminal envelope can bind the
    /// element bytes when nothing is queried below the indexed tree. A
    /// descent-shaped (`CountIndexedTree`) lower layer must be refused —
    /// there is no query at the lower path to verify it against, so the
    /// element bytes would go unbound.
    #[test]
    fn v1_indexed_terminal_rejects_non_terminal_lower_layer() {
        let v = GroveVersion::latest();
        let (_db, proof, path_query) = pcit_terminal_fixture(v);
        let mut root = decode_v1_root(&proof);
        {
            let terminal = lower_layer_at(&mut root, b"cidx");
            let bytes = match &terminal.merk_proof {
                ProofBytes::IndexedTreeTerminal(b) => b.clone(),
                _ => panic!("expected an IndexedTreeTerminal lower layer"),
            };
            terminal.merk_proof = ProofBytes::CountIndexedTree(bytes);
        }
        let tampered = encode_v1_root(root);

        let err = GroveDb::verify_query(&tampered, &path_query, v)
            .expect_err("a non-terminal lower layer must be rejected");
        assert!(
            matches!(
                err,
                Error::InvalidProof(_, ref msg)
                    if msg.contains("non-terminal lower layer for")
                        && msg.contains("indexed tree at key")
                        && msg.contains("element bytes would be unbound")
            ),
            "expected the non-terminal-envelope rejection, got {err:?}"
        );
    }

    /// verify.rs 824-833: a terminal attestation is a leaf of the proof
    /// tree; nested lower layers under it are never produced by the
    /// prover and would be silently ignored by the three-input check, so
    /// they are refused outright.
    #[test]
    fn v1_indexed_terminal_rejects_nested_lower_layers() {
        let v = GroveVersion::latest();
        let (_db, proof, path_query) = pcit_terminal_fixture(v);
        let mut root = decode_v1_root(&proof);
        {
            let terminal = lower_layer_at(&mut root, b"cidx");
            terminal.lower_layers.insert(
                b"smuggled".to_vec(),
                LayerProof {
                    merk_proof: ProofBytes::Merk(Vec::new()),
                    lower_layers: Default::default(),
                },
            );
        }
        let tampered = encode_v1_root(root);

        let err = GroveDb::verify_query(&tampered, &path_query, v)
            .expect_err("nested layers under a terminal attestation must be rejected");
        assert!(
            matches!(
                err,
                Error::InvalidProof(_, ref msg)
                    if msg.contains("indexed terminal attestation at key")
                        && msg.contains("must")
                        && msg.contains("not carry further lower layers")
            ),
            "expected the nested-lower-layer rejection, got {err:?}"
        );
    }

    /// verify.rs 834-845: the attestation payload is exactly
    /// `attestation || primary_root`. Any other length makes the 32/32
    /// split (and therefore the three-input hash) meaningless, so it is
    /// rejected before the split happens.
    #[test]
    fn v1_indexed_terminal_rejects_wrong_length_payload() {
        let v = GroveVersion::latest();
        for bad_len in [0usize, 32, 63, 65] {
            let (_db, proof, path_query) = pcit_terminal_fixture(v);
            let mut root = decode_v1_root(&proof);
            {
                let terminal = lower_layer_at(&mut root, b"cidx");
                terminal.merk_proof = ProofBytes::IndexedTreeTerminal(vec![7u8; bad_len]);
            }
            let tampered = encode_v1_root(root);

            let err = GroveDb::verify_query(&tampered, &path_query, v)
                .expect_err("a terminal attestation that is not 64 bytes must be rejected");
            assert!(
                matches!(
                    err,
                    Error::InvalidProof(_, ref msg)
                        if msg.contains("indexed terminal attestation at key")
                            && msg.contains("64 bytes")
                            && msg.contains(&format!("got {bad_len}"))
                ),
                "expected the 64-byte length rejection for length {bad_len}, got {err:?}"
            );
        }
    }

    // =================================================================
    // 3. V1 indexed descent walk: add-parent-tree push (verify.rs
    //    887-899), error propagation out of the primary recursion
    //    (verify.rs 934-945) and the limit-exhausted break
    //    (verify.rs 966-967).
    // =================================================================

    /// `[TEST_LEAF]/psit` = PSIT populated with `n` sum items, keyed
    /// `k0..k(n-1)` so descent order is predictable.
    fn psit_subquery_db(n: usize, grove_version: &GroveVersion) -> TempGroveDb {
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"psit",
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert PSIT");
        for i in 0..n {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"psit"].as_ref(),
                format!("k{i}").as_bytes(),
                Element::new_sum_item(i as i64),
                None,
                grove_version,
            )
            .unwrap()
            .expect("populate PSIT");
        }
        db
    }

    /// verify.rs 887-899: with `add_parent_tree_on_subquery`, the
    /// indexed-tree element crossed by the descent is pushed into the
    /// result set alongside the primary's items.
    #[test]
    fn v1_indexed_descent_adds_parent_tree_when_requested() {
        let v = GroveVersion::latest();
        let db = psit_subquery_db(3, v);

        let plain = key_subquery(b"psit", None, false);
        let plain_proof = db.prove_query(&plain, None, v).unwrap().expect("prove");
        let plain_items = assert_verifies_like_query_raw(&db, &plain_proof, &plain, v);
        assert_eq!(plain_items.len(), 3, "three PSIT rows without the parent");

        let with_parent = key_subquery(b"psit", None, true);
        let proof = db
            .prove_query(&with_parent, None, v)
            .unwrap()
            .expect("prove with add_parent_tree_on_subquery");
        let (root, items) =
            GroveDb::verify_query(&proof, &with_parent, v).expect("honest proof verifies");
        assert_eq!(root, db.root_hash(None, v).unwrap().expect("root"));
        assert_eq!(
            items.len(),
            plain_items.len() + 1,
            "the crossed PSIT element is added on top of the three rows"
        );
        let parent_entries: Vec<_> = items
            .iter()
            .filter(|(_, key, _)| key.as_slice() == b"psit")
            .collect();
        assert_eq!(
            parent_entries.len(),
            1,
            "exactly one parent-tree entry is pushed"
        );
        assert!(
            matches!(
                parent_entries[0].2,
                Some(Element::ProvableSumIndexedTree(..))
            ),
            "the pushed parent entry is the PSIT element itself, got {:?}",
            parent_entries[0].2
        );
    }

    /// verify.rs 934-945: an error raised while verifying the indexed
    /// primary must propagate out of the descent instead of being
    /// swallowed. The primary layer inherits the cidx layer's
    /// `lower_layers`, so a lower layer for a key the primary never
    /// returns trips the succinctness check one level down.
    #[test]
    fn v1_indexed_descent_propagates_primary_layer_error() {
        let v = GroveVersion::latest();
        let db = psit_subquery_db(3, v);
        let path_query = key_subquery(b"psit", None, false);
        let proof = db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove PSIT subquery");
        assert_verifies_like_query_raw(&db, &proof, &path_query, v);

        let mut root = decode_v1_root(&proof);
        {
            let cidx = lower_layer_at(&mut root, b"psit");
            assert!(
                matches!(cidx.merk_proof, ProofBytes::CountIndexedTree(_)),
                "descent into an indexed tree uses the CountIndexedTree envelope"
            );
            cidx.lower_layers.insert(
                b"not_a_primary_key".to_vec(),
                LayerProof {
                    merk_proof: ProofBytes::Merk(Vec::new()),
                    lower_layers: Default::default(),
                },
            );
        }
        let tampered = encode_v1_root(root);

        let err = GroveDb::verify_query(&tampered, &path_query, v)
            .expect_err("the primary recursion's error must surface");
        assert!(
            matches!(
                err,
                Error::InvalidProof(_, ref msg)
                    if msg.contains("V1 proof contains extra lower layer for key")
                        && msg.contains(&hex::encode(b"not_a_primary_key"))
            ),
            "expected the inner layer's succinctness rejection to propagate, got {err:?}"
        );
    }

    /// verify.rs 966-967: when the indexed descent consumes the last of
    /// the query limit, the parent-level walk breaks out instead of
    /// processing further keys.
    #[test]
    fn v1_indexed_descent_breaks_when_limit_exhausted() {
        let v = GroveVersion::latest();
        let db = psit_subquery_db(4, v);
        // A sibling that sorts after "psit" so the break actually has
        // something left to skip.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"zzz",
            Element::new_item(b"sibling".to_vec()),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert sibling");

        let mut inner = Query::new();
        inner.insert_all();
        let mut q = Query::new();
        q.insert_range_inclusive(b"a".to_vec()..=b"zzzz".to_vec());
        q.set_subquery(inner);
        let path_query =
            PathQuery::new(vec![TEST_LEAF.to_vec()], SizedQuery::new(q, Some(2), None));

        let proof = db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove limited descent");
        let items = assert_verifies_like_query_raw(&db, &proof, &path_query, v);
        assert_eq!(
            items.len(),
            2,
            "the limit is consumed entirely inside the PSIT descent"
        );
        assert!(
            items.iter().all(|(_, key, _)| key.as_slice() != b"zzz"),
            "the post-break sibling must not appear, got {:?}",
            items
                .iter()
                .map(|(_, k, _)| hex::encode(k))
                .collect::<Vec<_>>()
        );

        // Control: without the limit the very same query does reach the
        // sibling, so the break above is what cut the walk short rather
        // than the proof simply not containing "zzz".
        let mut inner_all = Query::new();
        inner_all.insert_all();
        let mut q_all = Query::new();
        q_all.insert_range_inclusive(b"a".to_vec()..=b"zzzz".to_vec());
        q_all.set_subquery(inner_all);
        let unlimited =
            PathQuery::new(vec![TEST_LEAF.to_vec()], SizedQuery::new(q_all, None, None));
        let unlimited_proof = db
            .prove_query(&unlimited, None, v)
            .unwrap()
            .expect("prove unlimited descent");
        let all_items = assert_verifies_like_query_raw(&db, &unlimited_proof, &unlimited, v);
        assert_eq!(all_items.len(), 5, "four PSIT rows plus the sibling");
        assert!(
            all_items.iter().any(|(_, key, _)| key.as_slice() == b"zzz"),
            "the sibling is reachable when the limit does not run out"
        );
    }

    // =================================================================
    // 4. Regular-tree lower layer with no query below it
    //    (verify.rs 1003-1011).
    // =================================================================

    /// verify.rs 1003-1011: a lower layer attached to a regular tree
    /// that the query does not descend into is refused. An honest
    /// prover only inserts lower layers where there is a subquery;
    /// accepting one here would skip both binding mechanisms (the
    /// `combine_hash` chain check and the `child_hash_verified`
    /// requirement), letting a prover substitute forged element bytes
    /// under a genuine root hash.
    #[test]
    fn v1_rejects_lower_layer_for_regular_tree_with_no_query_below() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"tree",
            Element::empty_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert tree");
        db.insert(
            [TEST_LEAF, b"tree"].as_ref(),
            b"inner",
            Element::new_item(b"v".to_vec()),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert inner item");

        let path_query = key_query(b"tree");
        let proof = db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove terminal tree");
        assert_verifies_like_query_raw(&db, &proof, &path_query, v);

        let mut root = decode_v1_root(&proof);
        root.lower_layers
            .get_mut(TEST_LEAF)
            .expect("TEST_LEAF layer")
            .lower_layers
            .insert(
                b"tree".to_vec(),
                LayerProof {
                    merk_proof: ProofBytes::Merk(Vec::new()),
                    lower_layers: Default::default(),
                },
            );
        let tampered = encode_v1_root(root);

        let err = GroveDb::verify_query(&tampered, &path_query, v)
            .expect_err("an unrequested lower layer under a tree result must be rejected");
        assert!(
            matches!(
                err,
                Error::InvalidProof(_, ref msg)
                    if msg.contains("V1 proof supplies a lower layer for tree at key")
                        && msg.contains("no query below it")
                        && msg.contains("element bytes would be")
            ),
            "expected the unbound-element rejection, got {err:?}"
        );
    }

    // =================================================================
    // 5. V1 empty indexed-tree terminal value hash
    //    (verify.rs 1185-1226).
    // =================================================================

    fn all_axis_subsets() -> Vec<Vec<u8>> {
        vec![
            vec![IndexAxis::Count.tag()],
            vec![IndexAxis::Sum.tag()],
            vec![IndexAxis::Avg.tag()],
            vec![IndexAxis::Count.tag(), IndexAxis::Sum.tag()],
            vec![IndexAxis::Count.tag(), IndexAxis::Avg.tag()],
            vec![IndexAxis::Sum.tag(), IndexAxis::Avg.tag()],
            vec![
                IndexAxis::Count.tag(),
                IndexAxis::Sum.tag(),
                IndexAxis::Avg.tag(),
            ],
        ]
    }

    /// verify.rs 1196-1211: an empty PCPSIT commits
    /// `combine_hash_three(H(value), NULL_HASH, axes_digest(zero_axes))`,
    /// where `zero_axes` is the element's own axes list with every
    /// secondary root hash zeroed. Every axis subset must verify, since
    /// the digest is taken over the configured axes rather than a fixed
    /// three-axis list.
    #[test]
    fn v1_empty_pcpsit_terminal_verifies_for_every_axis_subset() {
        let v = GroveVersion::latest();
        for tags in all_axis_subsets() {
            let db = make_test_grovedb(v);
            let axes: Vec<(u8, Option<Vec<u8>>)> = tags.iter().map(|t| (*t, None)).collect();
            db.insert(
                [TEST_LEAF].as_ref(),
                b"pcpsit",
                Element::empty_provable_count_provable_sum_indexed_tree(axes)
                    .expect("canonical axes"),
                None,
                None,
                v,
            )
            .unwrap()
            .expect("insert empty PCPSIT");

            let path_query = key_query(b"pcpsit");
            let proof = db
                .prove_query(&path_query, None, v)
                .unwrap()
                .expect("prove empty PCPSIT terminal");
            let items = assert_verifies_like_query_raw(&db, &proof, &path_query, v);
            assert_eq!(
                items.len(),
                1,
                "the empty PCPSIT is the single result for axes {tags:?}"
            );
            assert!(
                matches!(
                    items[0].2,
                    Some(Element::ProvableCountProvableSumIndexedTree(
                        None,
                        0,
                        0,
                        _,
                        _
                    ))
                ),
                "axes {tags:?}: expected the empty PCPSIT element, got {:?}",
                items[0].2
            );
        }
    }

    /// verify.rs 1214-1226: swapping the element bytes of an empty
    /// indexed terminal (here PSIT → PCIT, which would silently change
    /// the tree's type and aggregate for the caller) leaves the
    /// Merk-level chain intact — a `KVValueHash*` node commits only to
    /// (key, value_hash) — so the GroveDB layer has to recompute the
    /// three-input expected hash from the element bytes and reject.
    #[test]
    fn v1_empty_indexed_terminal_rejects_forged_element_bytes() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"psit",
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert empty PSIT");

        let path_query = key_query(b"psit");
        let proof = db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove empty PSIT terminal");
        assert_verifies_like_query_raw(&db, &proof, &path_query, v);

        let forged_bytes = Element::empty_provable_count_indexed_tree()
            .serialize(v)
            .expect("serialize empty PCIT");
        let expected =
            combine_hash_three(&value_hash(&forged_bytes).unwrap(), &NULL_HASH, &NULL_HASH)
                .unwrap();

        let mut root = decode_v1_root(&proof);
        forge_value_bytes_in_layer(
            root.lower_layers
                .get_mut(TEST_LEAF)
                .expect("TEST_LEAF layer"),
            b"psit",
            forged_bytes,
        );
        let tampered = encode_v1_root(root);

        let err = GroveDb::verify_query(&tampered, &path_query, v)
            .expect_err("forged empty indexed-tree element bytes must be rejected");
        match err {
            Error::InvalidProof(_, ref msg) => {
                assert!(
                    msg.contains("empty indexed-tree value hash mismatch at key"),
                    "expected the empty indexed-tree mismatch rejection, got {msg}"
                );
                assert!(
                    msg.contains(&hex::encode(expected)),
                    "the recomputed hash must be the three-input form {} — got {msg}",
                    hex::encode(expected)
                );
            }
            other => panic!("expected InvalidProof, got {other:?}"),
        }
    }

    // =================================================================
    // 6. V0 verifier refuses indexed-tree descent (verify.rs 2092-2099).
    // =================================================================

    fn to_v0_layer(layer: &LayerProof) -> MerkOnlyLayerProof {
        let merk_proof = match &layer.merk_proof {
            ProofBytes::Merk(b) => b.clone(),
            // The V0 envelope has no way to express these; the bytes are
            // never read because the descent is refused first.
            _ => Vec::new(),
        };
        MerkOnlyLayerProof {
            merk_proof,
            lower_layers: layer
                .lower_layers
                .iter()
                .map(|(k, l)| (k.clone(), to_v0_layer(l)))
                .collect(),
        }
    }

    /// verify.rs 2092-2099: V0 is a frozen wire format that cannot
    /// describe indexed-tree descent, and the V0 prover refuses to
    /// produce it. A hand-built V0 envelope that nonetheless carries a
    /// lower layer under an indexed-tree element must be refused by the
    /// verifier rather than have a chain fabricated for it.
    #[test]
    fn v0_verifier_rejects_indexed_tree_lower_layer() {
        let v = GroveVersion::latest();
        let db = psit_subquery_db(2, v);
        let path_query = key_subquery(b"psit", None, false);
        let proof = db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove V1 PSIT subquery");

        // Re-wrap the honest V1 layer tree as a V0 (merk-only) envelope:
        // the per-layer merk proof bytes use the same encoding, so the V0
        // walk reaches the PSIT element with a lower layer attached.
        let root_layer = to_v0_layer(&decode_v1_root(&proof));
        let downgraded = bincode::encode_to_vec(
            GroveDBProof::V0(GroveDBProofV0 {
                root_layer,
                prove_options: ProveOptions {
                    decrease_limit_on_empty_sub_query_result: true,
                },
            }),
            proof_cfg(),
        )
        .expect("encode V0 envelope");

        let err = GroveDb::verify_query_raw(&downgraded, &path_query, v)
            .expect_err("V0 must not descend into an indexed tree");
        assert!(
            matches!(
                err,
                Error::NotSupported(ref msg)
                    if msg.contains("V0 proofs do not support descent into any indexed-tree")
            ),
            "expected the V0 indexed-descent refusal, got {err:?}"
        );
    }
}
