//! Regression tests for a count-offset proof forgery through the
//! backward-references item types (found by the 2026-09-23 audit).
//!
//! An honest offset-paginated proof over a Provable* count tree returns
//! each in-range item as `KVCount(k, item, c)` (or `KVCountSum` on a
//! PCPS host), and the verifier recomputes `H(item)` from the bytes. A
//! proof supplier can re-emit that node as `KVValueHashFeatureType(k,
//! forged, H(item), <same aggregates>)`. That node hashes
//! `(key, value_hash, aggregates)` and takes `value_hash` from the proof,
//! so the node hash, and with it the root, does not change
//! (`kv_hash(k, v) == kv_digest_to_kv_hash(k, H(v))`).
//!
//! `classify_self` in `merk/src/proofs/query/count_offset/verify.rs` was
//! the only check on the element type of such a node, and it refused just
//! `Item` / `SumItem` / `ItemWithSumItem`. The count-offset verifier
//! rebuilds the tree with `execute_with_options`, so the refusal
//! `execute_proof` applies to the backward-references items never ran,
//! and the GroveDB post-filter in `run_count_offset_layer_dispatch` did
//! not look for them either. Every public verify entry point returned the
//! genuine root together with an attacker-chosen `ItemWithBackwardsReferences`
//! (arbitrary item bytes), `SumItemWithBackwardsReferences` (arbitrary sum)
//! or `ItemWithSumItemWithBackwardsReferences` (both). Provable* count
//! trees refuse these types at insert, so an honest proof never carries
//! one on a directly read row.
//!
//! The fix turns the merk guard into an allowlist (trees and references,
//! the only element types an honest prover puts on that node) and adds a
//! matching allowlist on unresolved rows in the GroveDB post-filter. Both
//! guards run on the V1 count-offset path only (V0 envelopes reject any
//! offset), so no grove-version gate is involved.
//!
//! Resolved reference rows stay exempt from the GroveDB-level allowlist:
//! a reference in a count tree may point at a backward-references item
//! elsewhere, and its row then carries that target's bytes, bound by the
//! `KVRefValueHash*` combine.
//! `end_to_end_offset_dereferences_bidirectional_references` in
//! `count_offset_paginated_tests.rs` pins that honest shape.

#[cfg(test)]
mod tests {
    use grovedb_merk::{
        proofs::{encode_into, tree::execute_with_options, Decoder, Node, Op},
        tree::{value_hash, TreeFeatureType},
        CryptoHash,
    };
    use grovedb_version::version::{v3::GROVE_V3, GroveVersion};

    use crate::{
        operations::proof::{GroveDBProof, GroveDBProofV1, ProofBytes},
        tests::{make_test_grovedb, TempGroveDb},
        BackwardReferences, Element, Error, GroveDb, PathQuery, Query, SizedQuery, VerifyOptions,
    };

    const HOST: &[u8] = b"host";
    /// The row every forgery rewrites. With 15 rows `a..=o`, offset 5 and
    /// limit 3 it is the last returned row ascending (`f, g, h`) and the
    /// last one descending (`j, i, h`).
    const TARGET: &[u8] = b"h";
    const FORGED_PAYLOAD: &[u8] = b"ATTACKER-CHOSEN DOCUMENT BYTES";
    const FORGED_SUM: i64 = 1_000_000_000;

    /// Message the merk-level allowlist in `classify_self` rejects with.
    const MERK_GUARD_MSG: &str = "KVValueHashFeatureType node carries an element of type";
    /// Message the GroveDB-level allowlist rejects with.
    const GROVEDB_GUARD_MSG: &str = "a count tree's own rows are simple items or empty trees";

    #[derive(Clone, Copy, Debug)]
    enum Host {
        /// `ProvableCountTree`
        Pct,
        /// `ProvableCountSumTree`
        Pcs,
        /// `ProvableCountProvableSumTree`
        Pcps,
    }

    impl Host {
        const ALL: [Host; 3] = [Host::Pct, Host::Pcs, Host::Pcps];

        fn empty_element(self) -> Element {
            match self {
                Host::Pct => Element::empty_provable_count_tree(),
                Host::Pcs => Element::empty_provable_count_sum_tree(),
                Host::Pcps => Element::empty_provable_count_provable_sum_tree(),
            }
        }
    }

    fn forged_elements() -> [(&'static str, Element); 3] {
        [
            (
                "ItemWithBackwardsReferences",
                Element::ItemWithBackwardsReferences(
                    FORGED_PAYLOAD.to_vec(),
                    BackwardReferences::default(),
                    None,
                ),
            ),
            (
                "SumItemWithBackwardsReferences",
                Element::SumItemWithBackwardsReferences(
                    FORGED_SUM,
                    BackwardReferences::default(),
                    None,
                ),
            ),
            (
                "ItemWithSumItemWithBackwardsReferences",
                Element::ItemWithSumItemWithBackwardsReferences(
                    FORGED_PAYLOAD.to_vec(),
                    FORGED_SUM,
                    BackwardReferences::default(),
                    None,
                ),
            ),
        ]
    }

    /// GROVE_V3 is live; latest is V4. The forgery reproduced under both.
    fn versions() -> [(&'static str, &'static GroveVersion); 2] {
        [("V3", &GROVE_V3), ("latest", GroveVersion::latest())]
    }

    /// A `host` count tree at the root holding plain items `a..=o`, and
    /// the offset-5, limit-3 page over `a..=o` in the given direction.
    fn fixture(host: Host, left_to_right: bool, gv: &GroveVersion) -> (TempGroveDb, PathQuery) {
        let db = make_test_grovedb(gv);
        db.insert(&[] as &[&[u8]], HOST, host.empty_element(), None, None, gv)
            .unwrap()
            .expect("insert host tree");
        for key in b'a'..=b'o' {
            db.insert(
                &[HOST],
                &[key],
                Element::new_item(format!("honest_{}", key as char).into_bytes()),
                None,
                None,
                gv,
            )
            .unwrap()
            .expect("insert item");
        }
        let mut query = Query::new_with_direction(left_to_right);
        query.insert_range_inclusive(b"a".to_vec()..=b"o".to_vec());
        let path_query = PathQuery::new(
            vec![HOST.to_vec()],
            SizedQuery::new(query, Some(3), Some(5)),
        );
        (db, path_query)
    }

    fn bincode_config() -> impl bincode::config::Config {
        bincode::config::standard()
            .with_big_endian()
            .with_no_limit()
    }

    fn decode_v1(proof: &[u8]) -> GroveDBProofV1 {
        let (decoded, _) = bincode::decode_from_slice::<GroveDBProof, _>(proof, bincode_config())
            .expect("decode proof envelope");
        match decoded {
            GroveDBProof::V1(v1) => v1,
            GroveDBProof::V0(_) => panic!("count-offset proofs use the V1 envelope"),
        }
    }

    fn host_layer_ops(proof: &GroveDBProofV1) -> Vec<Op> {
        let layer = proof
            .root_layer
            .lower_layers
            .get(HOST)
            .expect("host layer present");
        let ProofBytes::Merk(bytes) = &layer.merk_proof else {
            panic!("host layer must carry a merk proof");
        };
        Decoder::new(bytes)
            .map(|op| op.expect("decode proof op"))
            .collect()
    }

    /// The host merk root the given ops reconstruct, without any
    /// node-type or element-type checks.
    fn reconstructed_host_root(ops: &[Op]) -> CryptoHash {
        execute_with_options(
            ops.iter().map(|op| Ok(op.clone())),
            false,
            false,
            |_| Ok(()),
        )
        .unwrap()
        .expect("execute host layer ops")
        .hash()
        .unwrap()
    }

    fn replace_host_layer_ops(mut proof: GroveDBProofV1, ops: &[Op]) -> Vec<u8> {
        let mut bytes = Vec::new();
        encode_into(ops.iter(), &mut bytes);
        proof
            .root_layer
            .lower_layers
            .get_mut(HOST)
            .expect("host layer present")
            .merk_proof = ProofBytes::Merk(bytes);
        bincode::encode_to_vec(GroveDBProof::V1(proof), bincode_config())
            .expect("encode proof envelope")
    }

    /// Rewrite the `TARGET` row with `rewrite`, returning the re-encoded
    /// proof and the host ops before and after the rewrite.
    fn forge(honest: &[u8], rewrite: impl Fn(Node) -> Node) -> (Vec<u8>, Vec<Op>, Vec<Op>) {
        let proof = decode_v1(honest);
        let honest_ops = host_layer_ops(&proof);
        let mut replaced = 0;
        let mut rewrite_target = |node: Node| match node {
            Node::KVCount(..) | Node::KVCountSum(..) if node_key(&node) == TARGET => {
                replaced += 1;
                rewrite(node)
            }
            other => other,
        };
        // Descending pages push their nodes inverted.
        let forged_ops: Vec<Op> = honest_ops
            .iter()
            .cloned()
            .map(|op| match op {
                Op::Push(node) => Op::Push(rewrite_target(node)),
                Op::PushInverted(node) => Op::PushInverted(rewrite_target(node)),
                other => other,
            })
            .collect();
        assert_eq!(
            replaced, 1,
            "fixture: the honest proof must carry TARGET as exactly one KVCount / KVCountSum"
        );
        let forged = replace_host_layer_ops(proof, &forged_ops);
        (forged, honest_ops, forged_ops)
    }

    fn node_key(node: &Node) -> &[u8] {
        match node {
            Node::KVCount(key, ..) | Node::KVCountSum(key, ..) => key,
            _ => &[],
        }
    }

    /// The attack: carry `forged` on a `KVValueHashFeatureType` node that
    /// keeps the honest value hash and aggregates.
    fn as_feature_type_node(node: Node, forged: &[u8]) -> Node {
        match node {
            Node::KVCount(key, value, count) => Node::KVValueHashFeatureType(
                key,
                forged.to_vec(),
                value_hash(&value).unwrap(),
                TreeFeatureType::ProvableCountedMerkNode(count),
            ),
            Node::KVCountSum(key, value, count, sum) => Node::KVValueHashFeatureType(
                key,
                forged.to_vec(),
                value_hash(&value).unwrap(),
                TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(count, sum),
            ),
            other => panic!("unexpected node {other:?}"),
        }
    }

    /// Run the proof through every public entry point that serves a
    /// count-offset page and collect each outcome.
    fn verify_everywhere(
        proof: &[u8],
        path_query: &PathQuery,
        gv: &GroveVersion,
    ) -> Vec<(&'static str, Result<CryptoHash, Error>)> {
        vec![
            (
                "verify_query",
                GroveDb::verify_query(proof, path_query, gv).map(|(root, _)| root),
            ),
            (
                "verify_query_with_options",
                GroveDb::verify_query_with_options(
                    proof,
                    path_query,
                    VerifyOptions {
                        absence_proofs_for_non_existing_searched_keys: false,
                        verify_proof_succinctness: true,
                        include_empty_trees_in_result: false,
                    },
                    gv,
                )
                .map(|(root, _)| root),
            ),
            (
                "verify_query_raw",
                GroveDb::verify_query_raw(proof, path_query, gv).map(|(root, _)| root),
            ),
            (
                "verify_path_query",
                GroveDb::verify_path_query(proof, path_query, gv).map(|v| *v.root_hash()),
            ),
        ]
    }

    /// Every forged variant, on every host, under V3 and latest, in both
    /// directions, is refused by every entry point, and the refusal comes
    /// from the merk-level allowlist. The rewrite is checked to leave the
    /// host root unchanged, so the refusal is not a side effect of a hash
    /// mismatch.
    #[test]
    fn forged_backward_reference_rows_are_rejected_on_every_host_and_entry_point() {
        for (version_name, gv) in versions() {
            for host in Host::ALL {
                for left_to_right in [true, false] {
                    let (db, path_query) = fixture(host, left_to_right, gv);
                    let root = db.root_hash(None, gv).unwrap().expect("root hash");
                    let honest = db
                        .prove_query(&path_query, None, gv)
                        .unwrap()
                        .expect("prove honest page");
                    for (entry_point, outcome) in verify_everywhere(&honest, &path_query, gv) {
                        assert_eq!(
                            outcome.unwrap_or_else(|e| panic!(
                                "{version_name}/{host:?}/ltr={left_to_right}: honest proof \
                                 must verify via {entry_point}: {e:?}"
                            )),
                            root,
                            "{version_name}/{host:?}/ltr={left_to_right}: honest root via \
                             {entry_point}"
                        );
                    }

                    for (forged_name, forged_element) in forged_elements() {
                        let label =
                            format!("{version_name}/{host:?}/ltr={left_to_right}/{forged_name}");
                        let forged_bytes = forged_element
                            .serialize(gv)
                            .expect("serialize forged element");
                        let (forged, honest_ops, forged_ops) =
                            forge(&honest, |node| as_feature_type_node(node, &forged_bytes));
                        assert_eq!(
                            reconstructed_host_root(&forged_ops),
                            reconstructed_host_root(&honest_ops),
                            "{label}: the rewrite must preserve the host root, or this \
                             test does not exercise the forgery"
                        );

                        for (entry_point, outcome) in verify_everywhere(&forged, &path_query, gv) {
                            match outcome {
                                Err(Error::InvalidProof(_, message))
                                    if message.contains(MERK_GUARD_MSG) => {}
                                other => panic!(
                                    "{label}: {entry_point} must reject the forged row at the \
                                     merk-level allowlist; got {other:?}"
                                ),
                            }
                        }
                    }
                }
            }
        }
    }

    /// Control: the same rewrite carrying a plain `Item` is refused too.
    /// This was already refused before the fix and must stay refused under
    /// the allowlist.
    #[test]
    fn forged_plain_item_row_stays_rejected() {
        for (version_name, gv) in versions() {
            let (db, path_query) = fixture(Host::Pct, true, gv);
            let honest = db
                .prove_query(&path_query, None, gv)
                .unwrap()
                .expect("prove honest page");
            let forged_bytes = Element::new_item(FORGED_PAYLOAD.to_vec())
                .serialize(gv)
                .expect("serialize forged item");
            let (forged, _, _) = forge(&honest, |node| as_feature_type_node(node, &forged_bytes));
            match GroveDb::verify_query(&forged, &path_query, gv) {
                Err(Error::InvalidProof(_, message)) if message.contains(MERK_GUARD_MSG) => {}
                other => panic!(
                    "{version_name}: a forged Item row must be rejected at the merk-level \
                     allowlist; got {other:?}"
                ),
            }
        }
    }

    /// The GroveDB-level allowlist fires on its own. Here the forged bytes
    /// stay on a `KVCount` node, which the merk count-offset verifier
    /// accepts (it recomputes `H(value)` from the bytes, so the host root
    /// changes). The GroveDB post-filter runs before the parent layer
    /// compares that root with the committed one, so its refusal is the
    /// one reported.
    #[test]
    fn grovedb_allowlist_rejects_unresolved_backward_reference_rows() {
        for (version_name, gv) in versions() {
            for host in Host::ALL {
                let (db, path_query) = fixture(host, true, gv);
                let honest = db
                    .prove_query(&path_query, None, gv)
                    .unwrap()
                    .expect("prove honest page");
                for (forged_name, forged_element) in forged_elements() {
                    let label = format!("{version_name}/{host:?}/{forged_name}");
                    let forged_bytes = forged_element
                        .serialize(gv)
                        .expect("serialize forged element");
                    let (forged, _, _) = forge(&honest, |node| match node {
                        Node::KVCount(key, _, count) => {
                            Node::KVCount(key, forged_bytes.clone(), count)
                        }
                        Node::KVCountSum(key, _, count, sum) => {
                            Node::KVCountSum(key, forged_bytes.clone(), count, sum)
                        }
                        other => panic!("unexpected node {other:?}"),
                    });
                    match GroveDb::verify_query_raw(&forged, &path_query, gv) {
                        Err(Error::InvalidProof(_, message))
                            if message.contains(GROVEDB_GUARD_MSG) => {}
                        other => panic!(
                            "{label}: the GroveDB-level allowlist must reject an unresolved \
                             backward-references row; got {other:?}"
                        ),
                    }
                }
            }
        }
    }

    /// The premise of both allowlists: a Provable* count tree refuses the
    /// backward-references items at insert, so no honest proof carries one
    /// as a directly read row. If this starts failing, honest proofs may
    /// carry these rows and both allowlists need revisiting.
    #[test]
    fn count_hosts_refuse_backward_reference_items_at_insert() {
        let gv = GroveVersion::latest();
        for host in Host::ALL {
            let db = make_test_grovedb(gv);
            db.insert(&[] as &[&[u8]], HOST, host.empty_element(), None, None, gv)
                .unwrap()
                .expect("insert host tree");
            for (name, element) in forged_elements() {
                // A ProvableCountTree is not a sum tree, so it refuses the
                // two sum-carrying variants for that reason first.
                match db.insert(&[HOST], b"row", element, None, None, gv).unwrap() {
                    Err(Error::InvalidInput(message))
                        if message.contains("may not live in Provable* aggregate trees")
                            || message.contains("cannot add sum item to non sum tree") => {}
                    other => panic!(
                        "{host:?} must refuse a {name} at insert; the count-offset \
                         allowlists assume no honest row has this type. Got {other:?}"
                    ),
                }
            }
        }
    }
}
