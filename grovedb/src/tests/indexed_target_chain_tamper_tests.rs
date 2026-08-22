//! Adversarial coverage for the resolved-target chain an indexed-axis
//! proof carries.
//!
//! A chain hands the verifier the primary value a secondary row points at,
//! WITHOUT a per-row inclusion proof. The claim that makes that sound is
//! narrow and worth attacking directly: the row's own committed hash is
//! bound into the secondary root, and the chain reconstructs that hash
//! from its own bytes — so no substitution anywhere in the chain can
//! survive.
//!
//! Every test here takes an honest proof, decodes the envelope, changes
//! exactly one thing about a chain, re-encodes, and asserts verification
//! refuses. If any of these ever passes, the per-row saving is not free
//! and the design is wrong.

#[cfg(test)]
mod tests {
    use bincode::config::standard;
    use grovedb_element::reference_path::ReferencePathType;
    use grovedb_version::version::GroveVersion;

    use crate::{
        operations::proof::indexed_axis::{
            IndexedAxisRangeProof, IndexedTargetChain, IndexedTargetCommitment, IndexedTargetNode,
        },
        tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
        Element, GroveDb,
    };

    const PATH: [&[u8]; 2] = [TEST_LEAF, b"cidx"];

    /// A PCIT holding one item-shaped entry — the simplest chain, a single
    /// directly-valued node.
    fn db_with_item_entry(gv: &GroveVersion) -> TempGroveDb {
        let db = make_test_grovedb(gv);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("PCIT");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::new_item(b"honest-value".to_vec()),
            None,
            gv,
        )
        .unwrap()
        .expect("entry");
        db
    }

    /// A PCIT whose entry is a TREE — the layered commitment shape, which
    /// folds in a child root the element bytes do not carry.
    fn db_with_tree_entry(gv: &GroveVersion) -> TempGroveDb {
        let db = make_test_grovedb(gv);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("PCIT");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::empty_tree(),
            None,
            gv,
        )
        .unwrap()
        .expect("tree entry");
        db.insert(
            [TEST_LEAF, b"cidx", b"a"].as_ref(),
            b"k",
            Element::new_item(b"child".to_vec()),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("child");
        db
    }

    /// A PCIT whose entry is a REFERENCE — a two-node chain, head plus
    /// terminal.
    fn db_with_reference_entry(gv: &GroveVersion) -> TempGroveDb {
        let db = make_test_grovedb(gv);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"target",
            Element::new_item(b"terminal-value".to_vec()),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("terminal");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("PCIT");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"target".to_vec(),
            ])),
            None,
            gv,
        )
        .unwrap()
        .expect("reference entry");
        db
    }

    fn honest_proof(db: &TempGroveDb, gv: &GroveVersion) -> Vec<u8> {
        db.prove_indexed_count_top_k(PATH.as_ref(), 5, true, None, gv)
            .unwrap()
            .expect("prove")
    }

    /// Decode an envelope, let the caller rewrite its chains, re-encode.
    fn forge(proof: &[u8], mutate: impl FnOnce(&mut Vec<IndexedTargetChain>)) -> Vec<u8> {
        let (mut envelope, _): (IndexedAxisRangeProof, _) =
            bincode::decode_from_slice(proof, standard()).expect("decode envelope");
        mutate(&mut envelope.target_chains);
        bincode::encode_to_vec(&envelope, standard()).expect("re-encode")
    }

    #[track_caller]
    fn assert_rejected(proof: &[u8], gv: &GroveVersion, what: &str) {
        let res = GroveDb::verify_indexed_count_top_k(proof, PATH.as_ref(), 5, true, gv);
        assert!(
            res.is_err(),
            "verification accepted a proof with {what}; the chain is supposed to make that \
             impossible without a per-row inclusion proof"
        );
    }

    /// Like [`assert_rejected`], but pins WHICH refusal fired. Used where
    /// several guards could plausibly reject the same forgery and the test
    /// is only meaningful if the intended one is the guard that ran.
    #[track_caller]
    fn assert_rejected_because(proof: &[u8], gv: &GroveVersion, expected: &str, what: &str) {
        let res = GroveDb::verify_indexed_count_top_k(proof, PATH.as_ref(), 5, true, gv);
        let err = match res {
            Err(e) => e.to_string(),
            Ok(_) => panic!(
                "verification accepted a proof with {what}; the chain is supposed to make that \
                 impossible without a per-row inclusion proof"
            ),
        };
        assert!(
            err.contains(expected),
            "{what} was rejected, but by the wrong guard: expected a message containing \
             {expected:?}, got {err:?}"
        );
    }

    /// Baseline: the honest proofs verify, so the refusals below are
    /// caused by the tampering and not by a broken fixture.
    #[test]
    fn honest_chains_verify_for_every_target_shape() {
        let gv = GroveVersion::latest();
        for (label, db) in [
            ("item", db_with_item_entry(gv)),
            ("tree", db_with_tree_entry(gv)),
            ("reference", db_with_reference_entry(gv)),
        ] {
            let proof = honest_proof(&db, gv);
            let result = GroveDb::verify_indexed_count_top_k(&proof, PATH.as_ref(), 5, true, gv)
                .unwrap_or_else(|e| panic!("{label} chain must verify: {e}"));
            assert_eq!(
                result.root_hash,
                db.root_hash(None, gv).unwrap().unwrap(),
                "{label}: verified root must equal the live grove root"
            );
        }
    }

    /// Substituting the resolved VALUE is the attack the whole design
    /// stands on refusing. If a prover could swap this, a top-k result
    /// would carry an unauthenticated value.
    #[test]
    fn a_substituted_primary_value_is_rejected() {
        let gv = GroveVersion::latest();
        let db = db_with_item_entry(gv);
        let honest = honest_proof(&db, gv);
        let forged = forge(&honest, |chains| {
            chains[0].nodes[0].value = Element::new_item(b"attacker-value".to_vec())
                .serialize(gv)
                .expect("serialize");
        });
        assert_rejected(&forged, gv, "a substituted primary value");
    }

    /// Same attack one hop out: swap the TERMINAL a reference resolves to.
    #[test]
    fn a_substituted_terminal_value_is_rejected() {
        let gv = GroveVersion::latest();
        let db = db_with_reference_entry(gv);
        let honest = honest_proof(&db, gv);
        let forged = forge(&honest, |chains| {
            let last = chains[0].nodes.len() - 1;
            chains[0].nodes[last].value = Element::new_item(b"attacker-terminal".to_vec())
                .serialize(gv)
                .expect("serialize");
        });
        assert_rejected(&forged, gv, "a substituted terminal value");
    }

    /// Rewriting the reference head changes what the row committed to.
    #[test]
    fn a_substituted_reference_head_is_rejected() {
        let gv = GroveVersion::latest();
        let db = db_with_reference_entry(gv);
        let honest = honest_proof(&db, gv);
        let forged = forge(&honest, |chains| {
            chains[0].nodes[0].value =
                Element::new_reference(ReferencePathType::SiblingReference(b"elsewhere".to_vec()))
                    .serialize(gv)
                    .expect("serialize");
        });
        assert_rejected(&forged, gv, "a substituted reference head");
    }

    /// A layered target folds in a child root the element bytes do not
    /// carry. Claiming `Simple` drops that fold, so the reconstructed
    /// commitment would omit the subtree entirely.
    #[test]
    fn downgrading_a_layered_commitment_to_simple_is_rejected() {
        let gv = GroveVersion::latest();
        let db = db_with_tree_entry(gv);
        let honest = honest_proof(&db, gv);
        let forged = forge(&honest, |chains| {
            chains[0].nodes[0].commitment = IndexedTargetCommitment::Simple;
        });
        assert_rejected(&forged, gv, "a layered commitment downgraded to Simple");
    }

    /// Tampering the child root itself: the element bytes stay honest, so
    /// only the fold can catch this.
    #[test]
    fn a_tampered_layered_child_root_is_rejected() {
        let gv = GroveVersion::latest();
        let db = db_with_tree_entry(gv);
        let honest = honest_proof(&db, gv);
        let forged = forge(&honest, |chains| {
            chains[0].nodes[0].commitment = IndexedTargetCommitment::Layered([0xAB; 32]);
        });
        assert_rejected(&forged, gv, "a tampered layered child root");
    }

    /// Promoting a directly-valued node to `Reference` and appending a
    /// terminal would let a prover choose the returned value freely, since
    /// a reference commits its terminal rather than its own bytes.
    #[test]
    fn promoting_a_direct_head_to_a_reference_is_rejected() {
        let gv = GroveVersion::latest();
        let db = db_with_item_entry(gv);
        let honest = honest_proof(&db, gv);
        let forged = forge(&honest, |chains| {
            chains[0].nodes[0].commitment = IndexedTargetCommitment::Reference;
            chains[0].nodes.push(IndexedTargetNode {
                value: Element::new_item(b"attacker-value".to_vec())
                    .serialize(gv)
                    .expect("serialize"),
                commitment: IndexedTargetCommitment::Simple,
            });
        });
        assert_rejected(&forged, gv, "a direct head promoted to a reference");
    }

    /// A reference head with its terminal stripped. The head commits the
    /// terminal's hash, so without it the commitment cannot be rebuilt —
    /// and treating the head as directly-valued would be the wrong fold.
    #[test]
    fn a_reference_head_without_its_terminal_is_rejected() {
        let gv = GroveVersion::latest();
        let db = db_with_reference_entry(gv);
        let honest = honest_proof(&db, gv);
        let forged = forge(&honest, |chains| {
            chains[0].nodes.truncate(1);
        });
        assert_rejected(&forged, gv, "a reference head with no terminal");
    }

    /// The mirror of [`promoting_a_direct_head_to_a_reference_is_rejected`]:
    /// a terminal appended to a head left directly-valued. Only a reference
    /// resolves onward, so the extra entry must be refused rather than
    /// quietly taken as the resolved value.
    #[test]
    fn a_direct_head_carrying_a_terminal_is_rejected() {
        let gv = GroveVersion::latest();
        let db = db_with_item_entry(gv);
        let honest = honest_proof(&db, gv);
        let forged = forge(&honest, |chains| {
            chains[0].nodes.push(IndexedTargetNode {
                value: Element::new_item(b"attacker-value".to_vec())
                    .serialize(gv)
                    .expect("serialize"),
                commitment: IndexedTargetCommitment::Simple,
            });
        });
        assert_rejected_because(
            &forged,
            gv,
            "a directly-valued head carries a terminal",
            "a directly-valued head carrying a terminal",
        );
    }

    /// A terminal relabelled as a reference. A chain must END at a directly
    /// valued element — a reference terminal would commit a further hop
    /// that nothing in the proof binds.
    #[test]
    fn a_terminal_that_is_itself_a_reference_is_rejected() {
        let gv = GroveVersion::latest();
        let db = db_with_reference_entry(gv);
        let honest = honest_proof(&db, gv);
        let forged = forge(&honest, |chains| {
            chains[0].nodes[1].commitment = IndexedTargetCommitment::Reference;
        });
        assert_rejected_because(
            &forged,
            gv,
            "the terminal entry is itself a reference",
            "a terminal that is itself a reference",
        );
    }

    /// Padding a chain past head-plus-terminal. Extra entries are bound by
    /// nothing, so the shape is rejected rather than silently ignored.
    #[test]
    fn an_over_long_chain_is_rejected() {
        let gv = GroveVersion::latest();
        let db = db_with_reference_entry(gv);
        let honest = honest_proof(&db, gv);
        let forged = forge(&honest, |chains| {
            let extra = chains[0].nodes[chains[0].nodes.len() - 1].clone();
            chains[0].nodes.push(extra);
        });
        assert_rejected(&forged, gv, "an over-long chain");
    }

    /// An empty chain carries no primary at all.
    #[test]
    fn an_empty_chain_is_rejected() {
        let gv = GroveVersion::latest();
        let db = db_with_item_entry(gv);
        let honest = honest_proof(&db, gv);
        let forged = forge(&honest, |chains| {
            chains[0].nodes.clear();
        });
        assert_rejected(&forged, gv, "an empty chain");
    }

    /// Chains are matched to rows positionally, so a count mismatch must
    /// be refused rather than zipped short.
    #[test]
    fn a_chain_count_mismatch_is_rejected() {
        let gv = GroveVersion::latest();
        let db = db_with_item_entry(gv);
        let honest = honest_proof(&db, gv);
        let forged = forge(&honest, |chains| {
            chains.clear();
        });
        assert_rejected(&forged, gv, "fewer chains than rows");
    }

    /// Two entries, two rows: swapping their chains gives each row the
    /// other's value. Both chains are individually well-formed and both
    /// values are genuinely in the tree, so only the per-row binding
    /// catches this.
    #[test]
    fn swapping_two_rows_chains_is_rejected() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("PCIT");
        for (key, value) in [(b"a".as_ref(), b"first".as_ref()), (b"b", b"second")] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                key,
                Element::new_item(value.to_vec()),
                None,
                gv,
            )
            .unwrap()
            .expect("entry");
        }
        let honest = honest_proof(&db, gv);
        let (envelope, _): (IndexedAxisRangeProof, _) =
            bincode::decode_from_slice(&honest, standard()).expect("decode");
        assert_eq!(
            envelope.target_chains.len(),
            2,
            "fixture must produce two rows for the swap to mean anything"
        );

        let forged = forge(&honest, |chains| chains.swap(0, 1));
        assert_rejected(&forged, gv, "two rows' chains swapped");
    }
}
