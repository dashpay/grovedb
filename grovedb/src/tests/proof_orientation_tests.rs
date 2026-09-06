//! Issue #863: a query level's proof must be encoded in the op family of
//! the direction the verifier walks it in.
//!
//! The merk interpreter checks, per op, that upright pushes ascend and
//! inverted pushes descend — but it never ties the family to the walk
//! direction the query supplies, and it lets the two families mix in one
//! stream. `execute_proof`'s bound-witness logic reads "the previously
//! visited node" as "the neighbouring key in the tree", which only holds
//! when the visit order is the tree's in-order (all upright) or its exact
//! reverse (all inverted) *and* that is the order the walk expects. Break
//! either and an authentic root hash can carry a wrong absence, or fill a
//! descending page from the wrong end of the range.
//!
//! Every forgery here is either an honest proof of a *different* query
//! handed to the victim query, or a re-shuffled op stream that rebuilds
//! the honest tree byte-for-byte at the root. All must be rejected; the
//! honest counterparts must keep verifying and agree with trusted reads.

#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::{encode_into, Decoder, Node, Op};
    use grovedb_version::version::GroveVersion;

    use crate::{
        operations::proof::{GroveDBProof, LayerProof, ProofBytes},
        tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
        Element, Error, GroveDb, PathQuery, Query, SizedQuery,
    };

    const DOCS: &[u8] = b"docs";
    const KEYS: [&[u8]; 3] = [b"b", b"c", b"d"];

    /// `[TEST_LEAF, docs]` holding items `b`, `c`, `d`. Three keys in
    /// insertion order settle as root `c` with `b` and `d` as leaves.
    fn build_fixture(gv: &GroveVersion) -> TempGroveDb {
        let db = make_test_grovedb(gv);
        db.insert(
            [TEST_LEAF].as_ref(),
            DOCS,
            Element::empty_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("insert docs tree");
        for key in KEYS {
            db.insert(
                [TEST_LEAF, DOCS].as_ref(),
                key,
                Element::new_item(key.to_vec()),
                None,
                None,
                gv,
            )
            .unwrap()
            .expect("insert item");
        }
        db
    }

    fn docs_query(query: Query, limit: Option<u16>) -> PathQuery {
        PathQuery::new(
            vec![TEST_LEAF.to_vec(), DOCS.to_vec()],
            SizedQuery::new(query, limit, None),
        )
    }

    fn keys_query(keys: &[&[u8]], left_to_right: bool) -> PathQuery {
        let mut q = Query::new_with_direction(left_to_right);
        for key in keys {
            q.insert_key(key.to_vec());
        }
        docs_query(q, None)
    }

    fn full_range_query(left_to_right: bool, limit: Option<u16>) -> PathQuery {
        let mut q = Query::new_with_direction(left_to_right);
        q.insert_all();
        docs_query(q, limit)
    }

    fn trusted_keys(db: &TempGroveDb, pq: &PathQuery, gv: &GroveVersion) -> Vec<Vec<u8>> {
        db.query_item_value(pq, true, true, true, None, gv)
            .unwrap()
            .expect("trusted read")
            .0
    }

    fn prove(db: &TempGroveDb, pq: &PathQuery, gv: &GroveVersion) -> Vec<u8> {
        db.prove_query(pq, None, gv).unwrap().expect("prove")
    }

    fn verified_keys(
        proof: &[u8],
        pq: &PathQuery,
        gv: &GroveVersion,
    ) -> Result<Vec<Vec<u8>>, Error> {
        GroveDb::verify_query(proof, pq, gv).map(|(_, rows)| {
            rows.into_iter()
                .map(|(_, key, element)| {
                    assert!(element.is_some(), "rows carry their element");
                    key
                })
                .collect()
        })
    }

    /// Replaces the merk op stream of the `[TEST_LEAF, docs]` layer.
    fn rewrite_docs_layer_ops(proof: &[u8], mutate: impl FnOnce(Vec<Op>) -> Vec<Op>) -> Vec<u8> {
        let config = bincode::config::standard().with_big_endian();
        let (mut decoded, _): (GroveDBProof, usize) =
            bincode::decode_from_slice(proof, config).expect("decode envelope");
        let GroveDBProof::V1(ref mut v1) = decoded else {
            panic!("expected V1 envelope");
        };
        let docs_layer: &mut LayerProof = v1
            .root_layer
            .lower_layers
            .get_mut(TEST_LEAF)
            .expect("TEST_LEAF layer")
            .lower_layers
            .get_mut(DOCS)
            .expect("docs layer");
        let ProofBytes::Merk(bytes) = &docs_layer.merk_proof else {
            panic!("docs layer is a merk proof");
        };
        let ops: Vec<Op> = Decoder::new(bytes)
            .map(|op| op.expect("decode op"))
            .collect();
        let mut rewritten = Vec::new();
        encode_into(mutate(ops).iter(), &mut rewritten);
        docs_layer.merk_proof = ProofBytes::Merk(rewritten);
        bincode::encode_to_vec(&decoded, config).expect("re-encode envelope")
    }

    fn assert_rejected(result: Result<Vec<Vec<u8>>, Error>, what: &str) {
        match result {
            Err(Error::InvalidProof(..)) => {}
            other => panic!("{what} must be rejected as an invalid proof, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // Honest behaviour: both directions verify and agree with trusted
    // reads, limited and unlimited.
    // -----------------------------------------------------------------

    #[test]
    fn honest_proofs_agree_with_trusted_reads_in_both_directions() {
        let gv = GroveVersion::latest();
        let db = build_fixture(gv);
        for left_to_right in [true, false] {
            for limit in [None, Some(1), Some(2)] {
                let pq = full_range_query(left_to_right, limit);
                let expected = trusted_keys(&db, &pq, gv);
                let proof = prove(&db, &pq, gv);
                let got = verified_keys(&proof, &pq, gv).unwrap_or_else(|e| {
                    panic!("honest (ltr={left_to_right}, limit={limit:?}): {e}")
                });
                assert_eq!(got, expected, "ltr={left_to_right}, limit={limit:?}");
            }
            for key in KEYS {
                let pq = keys_query(&[key], left_to_right);
                let proof = prove(&db, &pq, gv);
                let got = verified_keys(&proof, &pq, gv)
                    .unwrap_or_else(|e| panic!("honest key {key:?} (ltr={left_to_right}): {e}"));
                assert_eq!(got, vec![key.to_vec()]);
            }
            // An honest absence in both directions.
            let pq = keys_query(&[b"bb"], left_to_right);
            let proof = prove(&db, &pq, gv);
            let got = verified_keys(&proof, &pq, gv)
                .unwrap_or_else(|e| panic!("honest absence (ltr={left_to_right}): {e}"));
            assert!(got.is_empty(), "bb is absent");
        }
    }

    // -----------------------------------------------------------------
    // Forgery 1: an honest proof of the opposite direction. The victim
    // query is a descending single-key read of `c` (present); the proof
    // is the honest ASCENDING proof of `{b, d}`, which abridges `c` to
    // its kv hash. Walked descending, `b` is met first and read as the
    // rightmost node of the tree, the `Key(c)` item is consumed as
    // "before this key", and `c` is reported absent.
    // -----------------------------------------------------------------

    #[test]
    fn ascending_proof_cannot_prove_a_descending_absence() {
        let gv = GroveVersion::latest();
        let db = build_fixture(gv);
        let victim = keys_query(&[b"c"], false);
        assert_eq!(trusted_keys(&db, &victim, gv), vec![b"c".to_vec()]);

        let forged = prove(&db, &keys_query(&[b"b", b"d"], true), gv);
        assert_rejected(
            verified_keys(&forged, &victim, gv),
            "an upright stream handed to a descending query",
        );
    }

    #[test]
    fn descending_proof_cannot_prove_an_ascending_absence() {
        let gv = GroveVersion::latest();
        let db = build_fixture(gv);
        let victim = keys_query(&[b"c"], true);
        assert_eq!(trusted_keys(&db, &victim, gv), vec![b"c".to_vec()]);

        let forged = prove(&db, &keys_query(&[b"b", b"d"], false), gv);
        assert_rejected(
            verified_keys(&forged, &victim, gv),
            "an inverted stream handed to an ascending query",
        );
    }

    // -----------------------------------------------------------------
    // Forgery 2: "give me the latest entry". A descending full range
    // with limit 1 reads `d`. The honest ASCENDING limit-1 proof reveals
    // `b` and abridges the rest — exactly the shape a satisfied limit
    // permits — so walked descending it fills the page with `b`.
    // -----------------------------------------------------------------

    #[test]
    fn ascending_limited_proof_cannot_fill_a_descending_page() {
        let gv = GroveVersion::latest();
        let db = build_fixture(gv);
        let victim = full_range_query(false, Some(1));
        assert_eq!(trusted_keys(&db, &victim, gv), vec![b"d".to_vec()]);

        let forged = prove(&db, &full_range_query(true, Some(1)), gv);
        assert_rejected(
            verified_keys(&forged, &victim, gv),
            "an upright limited stream handed to a descending page",
        );
    }

    #[test]
    fn descending_limited_proof_cannot_fill_an_ascending_page() {
        let gv = GroveVersion::latest();
        let db = build_fixture(gv);
        let victim = full_range_query(true, Some(1));
        assert_eq!(trusted_keys(&db, &victim, gv), vec![b"b".to_vec()]);

        let forged = prove(&db, &full_range_query(false, Some(1)), gv);
        assert_rejected(
            verified_keys(&forged, &victim, gv),
            "an inverted limited stream handed to an ascending page",
        );
    }

    // -----------------------------------------------------------------
    // Forgery 3: a MIXED stream in the query's own direction. Start from
    // the honest ascending proof of `{b, d}` (ops: Push(b) · Push(KVHash
    // c) · Parent · Push(d) · Child) and re-shuffle it as
    //
    //   Push(b) · Push(d) · PushInverted(KVHash c) · ParentInverted · Parent
    //
    // which rebuilds the very same tree (`ParentInverted` hangs `d` to
    // the right of `c`, `Parent` hangs `b` to its left) but visits `b`,
    // `d`, then the abridged `c`. Per-op key checks pass — `d > b` for
    // the upright family and a kv-hash node carries no key — yet the
    // ascending walk for `Key(c)` sees `b` and `d` as adjacent key
    // pushes and endorses the gap between them as an absence.
    // -----------------------------------------------------------------

    #[test]
    fn mixed_family_stream_cannot_prove_an_absence() {
        let gv = GroveVersion::latest();
        let db = build_fixture(gv);
        let victim = keys_query(&[b"c"], true);
        assert_eq!(trusted_keys(&db, &victim, gv), vec![b"c".to_vec()]);

        let honest = prove(&db, &keys_query(&[b"b", b"d"], true), gv);
        let forged = rewrite_docs_layer_ops(&honest, |ops| {
            let push_b = ops
                .iter()
                .find(|op| matches!(op, Op::Push(Node::KV(k, _)) if k.as_slice() == b"b"))
                .cloned()
                .expect("honest proof reveals b");
            let push_d = ops
                .iter()
                .find(|op| matches!(op, Op::Push(Node::KV(k, _)) if k.as_slice() == b"d"))
                .cloned()
                .expect("honest proof reveals d");
            let Some(Op::Push(Node::KVHash(c_kv_hash))) = ops
                .iter()
                .find(|op| matches!(op, Op::Push(Node::KVHash(_))))
                .cloned()
            else {
                panic!("honest proof abridges c to a kv hash");
            };
            vec![
                push_b,
                push_d,
                Op::PushInverted(Node::KVHash(c_kv_hash)),
                Op::ParentInverted,
                Op::Parent,
            ]
        });
        assert_rejected(verified_keys(&forged, &victim, gv), "a mixed-family stream");
    }

    // -----------------------------------------------------------------
    // Subset verification that stops at a tree element the proof
    // descended into: the lower layer is consumed for its root hash
    // only. Its direction is the generating query's, which the narrower
    // query does not know, so it is read off the stream — and a mixed
    // stream is still refused there.
    // -----------------------------------------------------------------

    fn docs_element_query() -> PathQuery {
        let mut q = Query::new();
        q.insert_key(DOCS.to_vec());
        PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], q)
    }

    #[test]
    fn subset_verification_reads_a_descending_lower_layer_for_its_root() {
        let gv = GroveVersion::latest();
        let db = build_fixture(gv);
        // A DESCENDING wide read, so the `docs` layer is emitted inverted.
        let wide = full_range_query(false, None);
        let proof = prove(&db, &wide, gv);
        let (root_hash, rows) = GroveDb::verify_subset_query(&proof, &docs_element_query(), gv)
            .expect("narrower query stops at the docs element");
        assert_eq!(
            root_hash,
            db.root_hash(None, gv).unwrap().expect("root hash")
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1, DOCS.to_vec());
        assert!(
            matches!(rows[0].2, Some(Element::Tree(..))),
            "the tree element itself is reported, got {:?}",
            rows[0].2
        );
    }

    #[test]
    fn subset_verification_refuses_a_mixed_lower_layer() {
        let gv = GroveVersion::latest();
        let db = build_fixture(gv);
        let proof = prove(&db, &full_range_query(false, None), gv);
        // Flip the family of the stream's last structural op. The
        // orientation read runs before any op is executed, so the
        // rejection is the mixed-family one, not a hash mismatch.
        let forged = rewrite_docs_layer_ops(&proof, |mut ops| {
            let last = ops
                .iter()
                .rposition(|op| matches!(op, Op::ParentInverted | Op::ChildInverted))
                .expect("descending stream has a structural op");
            ops[last] = match ops[last] {
                Op::ParentInverted => Op::Parent,
                Op::ChildInverted => Op::Child,
                _ => unreachable!(),
            };
            ops
        });
        match GroveDb::verify_subset_query(&forged, &docs_element_query(), gv) {
            Err(Error::InvalidProof(_, msg)) => assert!(
                msg.contains("root derivation") && msg.contains("mixes upright and inverted"),
                "expected the lower layer's orientation read to refuse the mixed stream, got: {msg}"
            ),
            other => panic!("a mixed lower layer must be rejected, got {other:?}"),
        }
    }

    /// The mixed stream above really is a faithful reconstruction: it
    /// must be rejected for its op families, not for its hash.
    #[test]
    fn mixed_family_stream_rebuilds_the_honest_root() {
        let gv = GroveVersion::latest();
        let db = build_fixture(gv);
        let honest = prove(&db, &keys_query(&[b"b", b"d"], true), gv);
        let config = bincode::config::standard().with_big_endian();
        let (decoded, _): (GroveDBProof, usize) =
            bincode::decode_from_slice(&honest, config).expect("decode envelope");
        let GroveDBProof::V1(v1) = decoded else {
            panic!("expected V1 envelope");
        };
        let ProofBytes::Merk(bytes) =
            &v1.root_layer.lower_layers[TEST_LEAF].lower_layers[DOCS].merk_proof
        else {
            panic!("docs layer is a merk proof");
        };
        let ops: Vec<Op> = Decoder::new(bytes).map(|op| op.expect("op")).collect();
        let honest_root =
            grovedb_merk::proofs::tree::execute(ops.iter().cloned().map(Ok), false, |_| Ok(()))
                .unwrap()
                .expect("honest ops execute")
                .hash()
                .unwrap();
        let Some(Op::Push(Node::KVHash(c_kv_hash))) = ops
            .iter()
            .find(|op| matches!(op, Op::Push(Node::KVHash(_))))
            .cloned()
        else {
            panic!("honest proof abridges c");
        };
        let mixed = vec![
            ops[0].clone(),
            ops.iter()
                .find(|op| matches!(op, Op::Push(Node::KV(k, _)) if k.as_slice() == b"d"))
                .cloned()
                .expect("d"),
            Op::PushInverted(Node::KVHash(c_kv_hash)),
            Op::ParentInverted,
            Op::Parent,
        ];
        let mixed_root =
            grovedb_merk::proofs::tree::execute(mixed.into_iter().map(Ok), false, |_| Ok(()))
                .unwrap()
                .expect("mixed ops execute")
                .hash()
                .unwrap();
        assert_eq!(mixed_root, honest_root);
    }
}
