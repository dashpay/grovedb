//! Per-axis proof envelopes in the two situations the flat single-tree tests
//! never reach: an indexed tree whose **ancestors are themselves indexed**, and
//! the numeric edges of the aggregate / pagination parameters.
//!
//! The ancestor chain is the interesting part. A verifier reconstructing the
//! GroveDB root walks each layer and recomputes what the parent recorded for
//! that layer's element. For a plain tree that is `combine_hash(H(value),
//! child_root)`, but an indexed ancestor commits a third input as well — the
//! lone secondary's root hash for PCIT/PSIT, or `axes_digest(...)` over the
//! whole canonical axes list for a PCPSIT. The prover has to attest which shape
//! each ancestor used, and the verifier has to chain accordingly. Nesting an
//! indexed tree under another indexed tree is what exercises those two
//! attestation shapes; with a flat `[leaf, tree]` layout every ancestor is
//! plain and only the `NotIndexed` arm ever runs.

#[cfg(test)]
mod tests {
    use grovedb_element::indexed::IndexAxis;
    use grovedb_version::version::GroveVersion;

    use crate::{
        operations::proof::indexed_axis::{
            AncestorAttestation, AxisEntries, IndexedAxisAggregateProof, IndexedAxisRangeProof,
        },
        tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
        Element, Error, GroveDb,
    };

    fn axes_tlv(axes: &[IndexAxis]) -> Vec<(u8, Option<Vec<u8>>)> {
        let mut tags: Vec<u8> = axes.iter().map(|a| a.tag()).collect();
        tags.sort_unstable();
        tags.dedup();
        tags.into_iter().map(|t| (t, None)).collect()
    }

    fn assert_clean(db: &TempGroveDb, gv: &GroveVersion) {
        let issues = db.verify_grovedb(None, true, true, gv).expect("verify");
        assert!(issues.is_empty(), "verify_grovedb issues: {issues:?}");
    }

    fn count_entries(entries: &AxisEntries) -> &Vec<(u64, Vec<u8>)> {
        match entries {
            AxisEntries::Count(v) => v,
            other => panic!("expected count entries, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // Indexed ancestors
    // -----------------------------------------------------------------

    /// `[test_leaf] / outer(PCIT) / inner(PCIT)`, proving the count axis of the
    /// inner tree.
    ///
    /// `outer` is an indexed ancestor, so its parent recorded
    /// `combine_hash_three(H(value), child_root, secondary_root)` rather than
    /// the two-input plain-tree composition. The proof must carry a
    /// `SingleSecondary` attestation for that layer and the verifier must chain
    /// through it — reconstructing the real GroveDB root hash is the assertion
    /// that it did, since any other chaining yields a different 32 bytes.
    #[test]
    fn a_pcit_nested_under_a_pcit_proves_through_a_single_secondary_ancestor() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("create outer PCIT");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer"].as_ref(),
            b"inner",
            Element::empty_provable_count_indexed_tree(),
            None,
            gv,
        )
        .unwrap()
        .expect("nest an empty PCIT inside the outer PCIT");
        // Give the outer index a second entry so its secondary is non-trivial
        // and an incorrectly chained ancestor cannot coincidentally match.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer"].as_ref(),
            b"sibling",
            Element::new_item(b"s".to_vec()),
            None,
            gv,
        )
        .unwrap()
        .expect("sibling entry");
        for key in [b"x".as_ref(), b"y".as_ref()] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"outer", b"inner"].as_ref(),
                key,
                Element::new_item(key.to_vec()),
                None,
                gv,
            )
            .unwrap()
            .expect("inner entry");
        }
        assert_clean(&db, gv);

        let path: &[&[u8]] = &[TEST_LEAF, b"outer", b"inner"];
        let proof = db
            .prove_indexed_count_top_k(path, 5, true, None, gv)
            .unwrap()
            .expect("prove inner count top_k");

        // The middle layer must be attested as a single-secondary indexed
        // ancestor; `NotIndexed` there would be the bug this covers.
        let (envelope, _): (IndexedAxisRangeProof, _) =
            bincode::decode_from_slice(&proof, bincode::config::standard())
                .expect("decode envelope");
        assert_eq!(envelope.layer_proofs.len(), 3, "one proof per path segment");
        assert!(
            matches!(
                envelope.ancestor_attestations.as_slice(),
                [
                    AncestorAttestation::NotIndexed,
                    AncestorAttestation::SingleSecondary(_)
                ]
            ),
            "expected [NotIndexed(test_leaf), SingleSecondary(outer)], got {:?}",
            envelope.ancestor_attestations
        );

        let result = GroveDb::verify_indexed_axis_top_k(&proof, path, IndexAxis::Count, 5, true)
            .expect("verify through an indexed ancestor");
        assert_eq!(
            result.root_hash,
            db.root_hash(None, gv).unwrap().expect("root hash"),
            "the reconstructed root must equal the live GroveDB root"
        );
        assert_eq!(
            count_entries(&result.entries),
            &vec![(1u64, b"y".to_vec()), (1u64, b"x".to_vec())],
            "descending count order, ties broken by descending key"
        );
    }

    /// The same shape one variant over: `[test_leaf] / outer(PSIT) / inner(PSIT)`,
    /// proving the sum axis.
    ///
    /// PCIT and PSIT ancestors share the `SingleSecondary` attestation but reach
    /// it down different arms — the prover has to recover *which* axis the
    /// ancestor indexes in order to open the right secondary namespace, and a
    /// count/sum mix-up there would open an empty namespace and attest the wrong
    /// root hash.
    #[test]
    fn a_psit_nested_under_a_psit_proves_through_a_sum_axis_ancestor() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("create outer PSIT");
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"outer"].as_ref(),
            b"inner",
            Element::empty_provable_sum_indexed_tree(),
            None,
            gv,
        )
        .unwrap()
        .expect("nest an empty PSIT inside the outer PSIT");
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"outer"].as_ref(),
            b"sibling",
            Element::new_sum_item(11),
            None,
            gv,
        )
        .unwrap()
        .expect("sibling entry");
        for (key, sum) in [(b"x".as_ref(), -4i64), (b"y".as_ref(), 9)] {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"outer", b"inner"].as_ref(),
                key,
                Element::new_sum_item(sum),
                None,
                gv,
            )
            .unwrap()
            .expect("inner entry");
        }
        assert_clean(&db, gv);

        let path: &[&[u8]] = &[TEST_LEAF, b"outer", b"inner"];
        let proof = db
            .prove_indexed_sum_top_k(path, 5, true, None, gv)
            .unwrap()
            .expect("prove inner sum top_k");

        let (envelope, _): (IndexedAxisRangeProof, _) =
            bincode::decode_from_slice(&proof, bincode::config::standard())
                .expect("decode envelope");
        assert!(
            matches!(
                envelope.ancestor_attestations.as_slice(),
                [
                    AncestorAttestation::NotIndexed,
                    AncestorAttestation::SingleSecondary(_)
                ]
            ),
            "expected [NotIndexed(test_leaf), SingleSecondary(outer)], got {:?}",
            envelope.ancestor_attestations
        );

        let result = GroveDb::verify_indexed_axis_top_k(&proof, path, IndexAxis::Sum, 5, true)
            .expect("verify through a sum-axis indexed ancestor");
        assert_eq!(
            result.root_hash,
            db.root_hash(None, gv).unwrap().expect("root hash"),
            "the reconstructed root must equal the live GroveDB root"
        );
        assert_eq!(
            result.entries,
            AxisEntries::Sum(vec![(9i64, b"y".to_vec()), (-4i64, b"x".to_vec())]),
            "descending sum order, negatives sorting below positives"
        );
    }

    /// `[test_leaf] / outer(PCPSIT over count+sum+avg) / inner(PCPSIT over count)`.
    ///
    /// A PCPSIT ancestor commits `axes_digest(...)` over its *whole* canonical
    /// axes list, so the attestation has to carry all three `(tag, root_hash)`
    /// pairs — dropping or reordering any of them changes the digest and breaks
    /// the chain. The inner tree indexes only the count axis, which also pins
    /// that the deepest layer still composes through `axes_digest` for a PCPSIT
    /// with a single-axis TLV rather than using the bare secondary root.
    #[test]
    fn a_pcpsit_nested_under_a_pcpsit_proves_through_a_multi_axis_ancestor() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        let outer_axes = [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg];
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::empty_provable_count_provable_sum_indexed_tree(axes_tlv(&outer_axes))
                .expect("canonical axes"),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("create outer PCPSIT");
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"outer"].as_ref(),
            b"inner",
            Element::empty_provable_count_provable_sum_indexed_tree(axes_tlv(&[IndexAxis::Count]))
                .expect("canonical axes"),
            None,
            gv,
        )
        .unwrap()
        .expect("nest an empty PCPSIT inside the outer PCPSIT");
        for (key, sum) in [(b"x".as_ref(), 3i64), (b"y".as_ref(), 8)] {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"outer", b"inner"].as_ref(),
                key,
                Element::new_item_with_sum_item(key.to_vec(), sum),
                None,
                gv,
            )
            .unwrap()
            .expect("inner entry");
        }
        assert_clean(&db, gv);

        let path: &[&[u8]] = &[TEST_LEAF, b"outer", b"inner"];
        let proof = db
            .prove_indexed_count_top_k(path, 5, true, None, gv)
            .unwrap()
            .expect("prove inner count top_k");

        let (envelope, _): (IndexedAxisRangeProof, _) =
            bincode::decode_from_slice(&proof, bincode::config::standard())
                .expect("decode envelope");
        assert!(
            envelope.target_is_pcpsit,
            "the queried target is a PCPSIT even with one axis in its TLV"
        );
        assert!(
            envelope.other_axes_root_hashes.is_empty(),
            "a single-axis PCPSIT has no other axes to carry"
        );
        match envelope.ancestor_attestations.as_slice() {
            [AncestorAttestation::NotIndexed, AncestorAttestation::MultiAxis(axes)] => {
                let tags: Vec<u8> = axes.iter().map(|(t, _)| *t).collect();
                assert_eq!(
                    tags,
                    axes_tlv(&outer_axes)
                        .into_iter()
                        .map(|(t, _)| t)
                        .collect::<Vec<u8>>(),
                    "the ancestor attestation must carry the outer tree's canonical axes list"
                );
            }
            other => panic!("expected [NotIndexed, MultiAxis], got {other:?}"),
        }

        let result = GroveDb::verify_indexed_axis_top_k(&proof, path, IndexAxis::Count, 5, true)
            .expect("verify through a multi-axis ancestor");
        assert_eq!(
            result.root_hash,
            db.root_hash(None, gv).unwrap().expect("root hash"),
            "the reconstructed root must equal the live GroveDB root"
        );
        assert_eq!(
            count_entries(&result.entries),
            &vec![(1u64, b"y".to_vec()), (1u64, b"x".to_vec())],
        );
    }

    /// Dropping one axis from a `MultiAxis` ancestor attestation must break the
    /// chain. This is what makes the previous test's root-hash equality load
    /// bearing: the ancestor's `axes_digest` is over the full list, so a
    /// verifier that accepted a truncated list would accept a proof rooted in a
    /// tree that never existed.
    #[test]
    fn dropping_an_axis_from_a_multi_axis_ancestor_attestation_breaks_the_chain() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        let outer_axes = [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg];
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::empty_provable_count_provable_sum_indexed_tree(axes_tlv(&outer_axes))
                .expect("canonical axes"),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("create outer PCPSIT");
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"outer"].as_ref(),
            b"inner",
            Element::empty_provable_count_provable_sum_indexed_tree(axes_tlv(&[IndexAxis::Count]))
                .expect("canonical axes"),
            None,
            gv,
        )
        .unwrap()
        .expect("nest inner PCPSIT");
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"outer", b"inner"].as_ref(),
            b"x",
            Element::new_item_with_sum_item(b"x".to_vec(), 3),
            None,
            gv,
        )
        .unwrap()
        .expect("inner entry");

        let path: &[&[u8]] = &[TEST_LEAF, b"outer", b"inner"];
        let proof = db
            .prove_indexed_count_top_k(path, 5, true, None, gv)
            .unwrap()
            .expect("prove");
        let config = bincode::config::standard();
        let (mut envelope, _): (IndexedAxisRangeProof, _) =
            bincode::decode_from_slice(&proof, config).expect("decode envelope");

        match &mut envelope.ancestor_attestations[1] {
            AncestorAttestation::MultiAxis(axes) => {
                assert_eq!(axes.len(), 3, "outer indexes three axes");
                axes.pop();
            }
            other => panic!("expected MultiAxis, got {other:?}"),
        }
        let forged = bincode::encode_to_vec(&envelope, config).expect("re-encode");

        let err = GroveDb::verify_indexed_axis_top_k(&forged, path, IndexAxis::Count, 5, true)
            .expect_err("a truncated axes list must not verify");
        match err {
            Error::CorruptedData(message) => assert!(
                message.contains("chain mismatch")
                    && message.contains("combine_hash_three(H(value), child_root, axes_digest)"),
                "unexpected message: {message}"
            ),
            other => panic!("expected CorruptedData, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // Numeric edges of the aggregate and pagination parameters
    // -----------------------------------------------------------------

    fn build_flat_pcit(db: &TempGroveDb, gv: &GroveVersion, keys: &[&[u8]]) {
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcit",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("create PCIT");
        for key in keys {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"pcit"].as_ref(),
                key,
                Element::new_item(key.to_vec()),
                None,
                gv,
            )
            .unwrap()
            .expect("entry");
        }
    }

    /// Count values are `u64`, so a requested lower bound below zero has no
    /// representable encoding. Prover and verifier must both clamp it to `0`
    /// and — crucially — clamp it *identically*, since the verifier rebuilds the
    /// secondary-keyspace range from the echoed `lo`/`hi` and any disagreement
    /// makes an honest proof fail to verify.
    #[test]
    fn a_negative_lower_bound_on_the_count_aggregate_clamps_to_zero_on_both_sides() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build_flat_pcit(&db, gv, &[b"a", b"b", b"c"]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];

        let proof = db
            .prove_indexed_axis_range_aggregate(path, IndexAxis::Count, -5, 10, None, gv)
            .unwrap()
            .expect("prove with a below-domain lower bound");
        let result =
            GroveDb::verify_indexed_axis_range_aggregate(&proof, path, IndexAxis::Count, -5, 10)
                .expect("verify with the same bounds");
        assert_eq!(
            result.aggregate, 3,
            "[-5, 10] must cover the same entries as [0, 10]"
        );
        assert_eq!(result.axis, IndexAxis::Count);
        assert_eq!(
            result.root_hash,
            db.root_hash(None, gv).unwrap().expect("root hash")
        );

        // Identical to the in-domain request it clamps to.
        let clamped = db
            .prove_indexed_axis_range_aggregate(path, IndexAxis::Count, 0, 10, None, gv)
            .unwrap()
            .expect("prove [0, 10]");
        assert_eq!(
            GroveDb::verify_indexed_axis_range_aggregate(&clamped, path, IndexAxis::Count, 0, 10)
                .expect("verify [0, 10]")
                .aggregate,
            result.aggregate,
        );

        // A range wholly below the domain is a different thing entirely: it must
        // commit zero rather than clamp onto the boundary and count the rows
        // sitting there.
        let below = db
            .prove_indexed_axis_range_aggregate(path, IndexAxis::Count, -20, -1, None, gv)
            .unwrap()
            .expect("prove a wholly out-of-domain range");
        assert_eq!(
            GroveDb::verify_indexed_axis_range_aggregate(&below, path, IndexAxis::Count, -20, -1)
                .expect("verify")
                .aggregate,
            0,
            "an entirely below-domain range must commit 0, not the count at 0"
        );
    }

    /// The sum axis has no count-bound offset primitive, so its paginated proof
    /// is a plain range proof limited to `offset + k`. That limit is a `u16`, and
    /// clamping an overflowing request would silently prove a short page while
    /// the documented `skipped == expected_offset` cross-check still passed — so
    /// the prover refuses instead.
    #[test]
    fn a_sum_axis_page_beyond_the_u16_proof_limit_is_refused_rather_than_truncated() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"psit",
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("create PSIT");
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"a",
            Element::new_sum_item(1),
            None,
            gv,
        )
        .unwrap()
        .expect("entry");
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];

        let err = db
            .prove_indexed_sum_top_k_paginated(path, 10, u16::MAX as u64, false, None, gv)
            .unwrap()
            .expect_err("offset + k overflows the u16 page limit");
        match err {
            Error::NotSupported(message) => assert!(
                message.starts_with("indexed-axis paginated proof (sum): offset + k = 65545")
                    && message.contains("65535 entry limit"),
                "unexpected message: {message}"
            ),
            other => panic!("expected NotSupported, got {other:?}"),
        }

        // One less is exactly at the limit and is accepted, which is what makes
        // the refusal above about the overflow rather than about large offsets.
        db.prove_indexed_sum_top_k_paginated(path, 10, u16::MAX as u64 - 10, false, None, gv)
            .unwrap()
            .expect("offset + k == u16::MAX must still be provable");
    }

    // -----------------------------------------------------------------
    // Envelope self-consistency checks in the verifier
    // -----------------------------------------------------------------

    /// The avg axis exists only on a PCPSIT. An envelope claiming a
    /// single-axis (PCIT/PSIT) target while asking to be read on the avg axis
    /// is self-contradictory: those targets compose the deepest layer from a
    /// bare secondary root, which no avg index ever produces. The verifier must
    /// reject the combination outright rather than attempt the composition.
    #[test]
    fn an_avg_envelope_claiming_a_single_axis_target_is_rejected() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            Element::empty_provable_count_provable_sum_indexed_tree(axes_tlv(&[IndexAxis::Avg]))
                .expect("canonical axes"),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("create PCPSIT");
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"pcpsit"].as_ref(),
            b"a",
            Element::new_item_with_sum_item(b"a".to_vec(), 6),
            None,
            gv,
        )
        .unwrap()
        .expect("entry");
        let path: &[&[u8]] = &[TEST_LEAF, b"pcpsit"];

        let proof = db
            .prove_indexed_avg_top_k(path, 5, true, None, gv)
            .unwrap()
            .expect("prove avg top_k");
        GroveDb::verify_indexed_axis_top_k(&proof, path, IndexAxis::Avg, 5, true)
            .expect("the honest avg proof verifies");

        let config = bincode::config::standard();
        let (mut envelope, _): (IndexedAxisRangeProof, _) =
            bincode::decode_from_slice(&proof, config).expect("decode envelope");
        assert!(
            envelope.target_is_pcpsit,
            "an avg target is always a PCPSIT"
        );
        envelope.target_is_pcpsit = false;
        let forged = bincode::encode_to_vec(&envelope, config).expect("re-encode");

        let err = GroveDb::verify_indexed_axis_top_k(&forged, path, IndexAxis::Avg, 5, true)
            .expect_err("avg on a claimed single-axis target must be refused");
        match err {
            Error::CorruptedData(message) => assert!(
                message.contains(
                    "Avg axis is only valid on a ProvableCountProvableSumIndexedTree \
                     (target_is_pcpsit=true)"
                ),
                "unexpected message: {message}"
            ),
            other => panic!("expected CorruptedData, got {other:?}"),
        }
    }

    /// Aggregates are undefined on the avg axis — averaging averages over a
    /// range is not closed form — so the verifier refuses an avg-tagged
    /// aggregate envelope even though no prover can emit one.
    #[test]
    fn an_aggregate_envelope_tagged_with_the_avg_axis_is_refused() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build_flat_pcit(&db, gv, &[b"a", b"b"]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];

        // The prover refuses up front.
        let prove_err = db
            .prove_indexed_axis_range_aggregate(path, IndexAxis::Avg, 0, 10, None, gv)
            .unwrap()
            .expect_err("no avg aggregate proof exists");
        assert!(
            matches!(prove_err, Error::NotSupported(ref m)
                if m == "indexed-axis aggregate proofs are not defined for the Avg axis"),
            "unexpected prover error: {prove_err:?}"
        );

        // Relabel a count aggregate envelope so the axis-tag echo check passes
        // and the verifier reaches its own axis-support rule.
        let proof = db
            .prove_indexed_axis_range_aggregate(path, IndexAxis::Count, 0, 10, None, gv)
            .unwrap()
            .expect("prove count aggregate");
        let config = bincode::config::standard();
        let (mut envelope, _): (IndexedAxisAggregateProof, _) =
            bincode::decode_from_slice(&proof, config).expect("decode envelope");
        envelope.axis_tag = IndexAxis::Avg.tag();
        let forged = bincode::encode_to_vec(&envelope, config).expect("re-encode");

        let err =
            GroveDb::verify_indexed_axis_range_aggregate(&forged, path, IndexAxis::Avg, 0, 10)
                .expect_err("an avg-tagged aggregate must be refused");
        assert!(
            matches!(err, Error::NotSupported(ref m)
                if m == "indexed-axis aggregate proofs are not defined for the Avg axis"),
            "unexpected verifier error: {err:?}"
        );
    }

    /// A corrupted secondary proof must fail inside the merk verifier rather
    /// than propagate a bogus secondary root into the layer chain — where it
    /// would surface as a confusing deepest-layer mismatch instead of naming
    /// the actual problem.
    #[test]
    fn a_corrupted_secondary_proof_is_reported_as_a_secondary_failure() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build_flat_pcit(&db, gv, &[b"a", b"b", b"c"]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];

        let proof = db
            .prove_indexed_count_top_k(path, 3, true, None, gv)
            .unwrap()
            .expect("prove");
        let config = bincode::config::standard();
        let (mut envelope, _): (IndexedAxisRangeProof, _) =
            bincode::decode_from_slice(&proof, config).expect("decode envelope");
        let last = envelope.secondary_proof.len() - 1;
        envelope.secondary_proof[last] ^= 0xff;
        let forged = bincode::encode_to_vec(&envelope, config).expect("re-encode");

        let err = GroveDb::verify_indexed_axis_top_k(&forged, path, IndexAxis::Count, 3, true)
            .expect_err("a mangled secondary proof must not verify");
        match err {
            Error::CorruptedData(message) => assert!(
                message.contains("secondary proof failed to verify"),
                "unexpected message: {message}"
            ),
            other => panic!("expected CorruptedData, got {other:?}"),
        }
    }
}
