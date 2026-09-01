//! Byte-level pins between `prove_query` on single-path axis queries and
//! the direct indexed-axis provers (issue #835).
//!
//! Platform's switch from the direct `prove_indexed_*` / `verify_indexed_*`
//! entry points to the unified `prove_query` / `verify_path_query` surface
//! is meant to be a pure refactor. These tests pin what "pure" means at the
//! byte level, so any genuine divergence is a documented fact rather than a
//! CI surprise.
//!
//! ## The documented divergence: outer envelopes are NOT byte-identical
//!
//! The two surfaces deliberately emit different wire formats, so whole-proof
//! byte-equality does not hold and never did:
//!
//! | | direct provers | `prove_query` |
//! | --- | --- | --- |
//! | envelope type | `IndexedAxisPaginatedProof` / `IndexedAxisRangeProof` | `GroveDBProof::V1` |
//! | bincode config | `standard()` (little-endian) | `standard().with_big_endian()` |
//! | path attestation | `layer_proofs` (single-key Merk proof per segment) + `ancestor_attestations` | ordinary `LayerProof` nesting of the general proof walk |
//! | echoed parameters | axis, k / offset / limit, direction — echoed and authenticated by the verifier | only `axis_tag` (plus the rank for `RankOfKey`); everything else is query-as-input |
//!
//! Because the formats are disjoint, "mutual acceptance" between the two
//! verifier entry points resolves as mutual REJECTION: each verifier cleanly
//! errors on the other's bytes (pinned below), so the two families cannot be
//! cross-fed accidentally. Platform must switch prover and verifier together
//! — which it owns on both sides, and which the issue anticipates.
//!
//! There is also one CAPABILITY divergence (divergence 2): a **bounded read
//! over a completely empty secondary**. The unified prover carries the empty
//! secondary as empty proof bytes, which the verifier resolves to a
//! NULL_HASH secondary root — the parent binding then attests the emptiness.
//! The standalone range prover has no empty-tree shape and refuses with the
//! merk-level "Cannot create proof for empty tree" (both pinned below). The
//! paginated shape has no such gap: both surfaces prove empty secondaries.
//!
//! ## What IS byte-identical: the semantic core
//!
//! Both surfaces are built over the same engines, and these tests pin that
//! the security-relevant payload is byte-for-byte shared, for the same state
//! and arguments, across all three axes, both traversal shapes, both
//! directions, empty and populated secondaries (except the bounded-over-empty
//! case, where only the unified surface can prove at all — divergence 2
//! above), and offset 0 / mid / past-the-end:
//!
//! - `secondary_proof` — the encoded Merk proof over the per-axis secondary
//!   (count-offset paginated for top-k, plain range for bounded),
//! - `target_chains` — the resolved primary rows,
//! - `primary_root_hash`, `other_axes_root_hashes`, `target_is_pcpsit`,
//!   `axis_tag`.
//!
//! And at the observable level: both verifiers reconstruct the same GroveDB
//! root hash and return identical entries (and identical attested `skipped`
//! for the paginated shape). Both provers are also deterministic — proving
//! twice yields identical bytes — which is what makes these goldens stable.
//!
//! The book documents the same relationship in
//! `docs/book/src/unified-path-query.md` ("Relationship to the specialized
//! surfaces").

#[cfg(test)]
mod tests {
    use grovedb_element::indexed::AVG_FIXED_POINT_SCALE;
    use grovedb_merk::proofs::{query::AxisQuery, Query as MerkQuery};
    use grovedb_query::IndexAxis;
    use grovedb_version::version::GroveVersion;

    use crate::{
        operations::proof::{
            indexed_axis::{
                AxisEntries, IndexedAxisPaginatedProof, IndexedAxisQueryResult,
                IndexedAxisRangeProof,
            },
            AxisDescentProof, GroveDBProof, LayerProof, ProofBytes, VerifiedPathQuery,
        },
        query::axis_lowering::axis_bounded_merk_query,
        tests::{make_test_grovedb, TEST_LEAF},
        Element, Error, GroveDb, PathQuery,
    };

    // -----------------------------------------------------------------
    // Fixtures (same shapes as the axis-descent and standalone suites)
    // -----------------------------------------------------------------

    /// Sums include a tie (40) and a negative so the directional walk's
    /// tie-break and the sum axis's signed ordering are both exercised.
    /// Every entry is an `ItemWithSumItem`, so counts are all 1 and the
    /// avg axis degenerates to `sum * AVG_FIXED_POINT_SCALE`.
    const ENTRIES: &[(&[u8], i64)] = &[
        (b"alice", 40),
        (b"bob", -10),
        (b"carol", 25),
        (b"dave", 40),
        (b"erin", 5),
    ];

    const ALL_AXES: [IndexAxis; 3] = [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg];

    /// PCPSIT with all three axes, so one fixture serves the whole axis
    /// matrix and `other_axes_root_hashes` is non-trivial on every axis.
    fn build_pcpsit(db: &GroveDb, grove_version: &GroveVersion, entries: &[(&[u8], i64)]) {
        let axes: Vec<(u8, Option<Vec<u8>>)> = vec![(0, None), (1, None), (2, None)];
        let elem =
            Element::empty_provable_count_provable_sum_indexed_tree(axes).expect("axes canonical");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            elem,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PCPSIT");
        for (k, sum) in entries {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                k,
                Element::new_item_with_sum_item(b"v".to_vec(), *sum),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert PCPSIT entry");
        }
    }

    /// PSIT: the single-secondary target shape (`target_is_pcpsit = false`,
    /// empty `other_axes_root_hashes`).
    fn build_psit(db: &GroveDb, grove_version: &GroveVersion, entries: &[(&[u8], i64)]) {
        db.insert(
            [TEST_LEAF].as_ref(),
            b"psit",
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PSIT");
        for (k, s) in entries {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"psit"].as_ref(),
                k,
                Element::new_sum_item(*s),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert PSIT entry");
        }
    }

    fn pcpsit_path() -> Vec<Vec<u8>> {
        vec![TEST_LEAF.to_vec(), b"pcpsit".to_vec()]
    }

    fn root_hash(db: &GroveDb, grove_version: &GroveVersion) -> [u8; 32] {
        db.root_hash(None, grove_version).unwrap().expect("root")
    }

    /// Inclusive per-axis bounds that select the three middle sums
    /// (5, 25, 40, 40 — everything but bob's -10) on the fixture above.
    /// On the count axis every entry has count 1, so `[1, 1]` selects all
    /// five — a full-population band with key-order ties.
    fn bounds_for(axis: IndexAxis) -> (i128, i128) {
        match axis {
            IndexAxis::Count => (1, 1),
            IndexAxis::Sum => (0, 40),
            IndexAxis::Avg => (0, 40 * AVG_FIXED_POINT_SCALE),
        }
    }

    // -----------------------------------------------------------------
    // Envelope plumbing
    // -----------------------------------------------------------------

    /// Walk the unified `GroveDBProof::V1` envelope and decode its single
    /// axis-descent payload.
    fn extract_descent_payload(proof: &[u8]) -> AxisDescentProof {
        let config = bincode::config::standard().with_big_endian();
        let (decoded, _): (GroveDBProof, usize) =
            bincode::decode_from_slice(proof, config).expect("decode unified envelope");
        let GroveDBProof::V1(v1) = decoded else {
            panic!("expected a V1 envelope");
        };
        fn find_descent(layer: &LayerProof) -> Option<&LayerProof> {
            if matches!(layer.merk_proof, ProofBytes::IndexedTreeAxisDescent(_)) {
                return Some(layer);
            }
            layer.lower_layers.values().find_map(find_descent)
        }
        let descent = find_descent(&v1.root_layer).expect("envelope has an axis descent");
        let ProofBytes::IndexedTreeAxisDescent(bytes) = &descent.merk_proof else {
            unreachable!();
        };
        AxisDescentProof::decode_canonical(bytes).expect("decode axis-descent payload")
    }

    fn decode_paginated_envelope(bytes: &[u8]) -> IndexedAxisPaginatedProof {
        let config = bincode::config::standard();
        let (envelope, consumed): (IndexedAxisPaginatedProof, usize) =
            bincode::decode_from_slice(bytes, config).expect("decode direct paginated envelope");
        assert_eq!(
            consumed,
            bytes.len(),
            "direct paginated envelope has trailing bytes"
        );
        envelope
    }

    fn decode_range_envelope(bytes: &[u8]) -> IndexedAxisRangeProof {
        let config = bincode::config::standard();
        let (envelope, consumed): (IndexedAxisRangeProof, usize) =
            bincode::decode_from_slice(bytes, config).expect("decode direct range envelope");
        assert_eq!(
            consumed,
            bytes.len(),
            "direct range envelope has trailing bytes"
        );
        envelope
    }

    /// The unified verifier's typed axis answer.
    fn verify_unified(
        proof: &[u8],
        path_query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> ([u8; 32], AxisEntries, Option<u64>) {
        match GroveDb::verify_path_query(proof, path_query, grove_version)
            .expect("unified proof verifies")
        {
            VerifiedPathQuery::AxisEntries {
                root_hash,
                entries,
                skipped,
            } => (root_hash, entries, skipped),
            other => panic!("expected AxisEntries, got {other:?}"),
        }
    }

    /// Per-axis direct bounded prover, fallible — the empty-secondary
    /// test pins its refusal.
    fn prove_direct_bounded_result(
        db: &GroveDb,
        path: &[&[u8]],
        axis: IndexAxis,
        secondary_query: MerkQuery,
        limit: u16,
        grove_version: &GroveVersion,
    ) -> Result<Vec<u8>, Error> {
        match axis {
            IndexAxis::Count => db.prove_indexed_count_query(
                path,
                secondary_query,
                Some(limit),
                None,
                grove_version,
            ),
            IndexAxis::Sum => {
                db.prove_indexed_sum_query(path, secondary_query, Some(limit), None, grove_version)
            }
            IndexAxis::Avg => {
                db.prove_indexed_avg_query(path, secondary_query, Some(limit), None, grove_version)
            }
        }
        .unwrap()
    }

    /// Per-axis direct bounded prover, as platform calls it today.
    fn prove_direct_bounded(
        db: &GroveDb,
        path: &[&[u8]],
        axis: IndexAxis,
        secondary_query: MerkQuery,
        limit: u16,
        grove_version: &GroveVersion,
    ) -> Vec<u8> {
        prove_direct_bounded_result(db, path, axis, secondary_query, limit, grove_version)
            .expect("direct bounded prove")
    }

    /// Per-axis direct bounded verifier, as platform calls it today.
    fn verify_direct_bounded(
        bytes: &[u8],
        path: &[&[u8]],
        axis: IndexAxis,
        secondary_query: MerkQuery,
        limit: u16,
        grove_version: &GroveVersion,
    ) -> IndexedAxisQueryResult {
        match axis {
            IndexAxis::Count => GroveDb::verify_indexed_count_query(
                bytes,
                path,
                secondary_query,
                Some(limit),
                grove_version,
            ),
            IndexAxis::Sum => GroveDb::verify_indexed_sum_query(
                bytes,
                path,
                secondary_query,
                Some(limit),
                grove_version,
            ),
            IndexAxis::Avg => GroveDb::verify_indexed_avg_query(
                bytes,
                path,
                secondary_query,
                Some(limit),
                grove_version,
            ),
        }
        .expect("direct bounded verify")
    }

    /// Pin the byte-identical semantic core shared by the unified descent
    /// payload and a direct envelope's fields.
    #[allow(clippy::too_many_arguments)]
    fn assert_semantic_core_eq(
        label: &str,
        payload: &AxisDescentProof,
        axis_tag: u8,
        target_is_pcpsit: bool,
        other_axes_root_hashes: &[(u8, [u8; 32])],
        primary_root_hash: &[u8; 32],
        secondary_proof: &[u8],
        target_chains: &[crate::operations::proof::indexed_axis::IndexedTargetChain],
    ) {
        assert_eq!(payload.axis_tag, axis_tag, "{label}: axis_tag");
        assert_eq!(
            payload.target_is_pcpsit, target_is_pcpsit,
            "{label}: target_is_pcpsit"
        );
        assert_eq!(
            payload.other_axes_root_hashes, other_axes_root_hashes,
            "{label}: other_axes_root_hashes"
        );
        assert_eq!(
            &payload.primary_root_hash, primary_root_hash,
            "{label}: primary_root_hash"
        );
        assert_eq!(
            payload.secondary_proof, secondary_proof,
            "{label}: secondary_proof bytes"
        );
        assert_eq!(
            payload.target_chains, target_chains,
            "{label}: target_chains"
        );
    }

    // -----------------------------------------------------------------
    // Top-k-paginated: axes x directions x offsets on a populated PCPSIT
    // -----------------------------------------------------------------

    #[test]
    fn paginated_envelopes_share_the_semantic_core_across_axes_directions_and_offsets() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcpsit(&db, grove_version, ENTRIES);
        let root = root_hash(&db, grove_version);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcpsit"];
        let k: u16 = 2;

        for axis in ALL_AXES {
            for descending in [true, false] {
                // offset 0 (first page), 2 (mid), 100 (past the end of 5).
                for offset in [0u64, 2, 100] {
                    let label = format!("({axis:?}, descending={descending}, offset={offset})");

                    let path_query =
                        PathQuery::new_axis_top_k(pcpsit_path(), axis, k, offset, descending);
                    let unified = db
                        .prove_query(&path_query, None, grove_version)
                        .unwrap()
                        .unwrap_or_else(|e| panic!("{label}: unified prove: {e}"));
                    let direct = db
                        .prove_indexed_axis_top_k_paginated(
                            path,
                            axis,
                            k,
                            offset,
                            descending,
                            None,
                            grove_version,
                        )
                        .unwrap()
                        .unwrap_or_else(|e| panic!("{label}: direct prove: {e}"));

                    // Both provers are deterministic: the goldens are stable.
                    let unified_again = db
                        .prove_query(&path_query, None, grove_version)
                        .unwrap()
                        .expect("unified re-prove");
                    assert_eq!(
                        unified, unified_again,
                        "{label}: unified prover determinism"
                    );
                    let direct_again = db
                        .prove_indexed_axis_top_k_paginated(
                            path,
                            axis,
                            k,
                            offset,
                            descending,
                            None,
                            grove_version,
                        )
                        .unwrap()
                        .expect("direct re-prove");
                    assert_eq!(direct, direct_again, "{label}: direct prover determinism");

                    // The documented divergence: outer envelopes differ.
                    assert_ne!(
                        unified, direct,
                        "{label}: outer envelopes are documented as different formats; if they \
                         became byte-identical, the module doc enumeration is stale"
                    );

                    // The pinned equality: the semantic core is byte-identical.
                    let payload = extract_descent_payload(&unified);
                    let envelope = decode_paginated_envelope(&direct);
                    assert_semantic_core_eq(
                        &label,
                        &payload,
                        axis.tag(),
                        true,
                        &envelope.other_axes_root_hashes,
                        &envelope.primary_root_hash,
                        &envelope.secondary_proof,
                        &envelope.target_chains,
                    );
                    assert_eq!(payload.rank, None, "{label}: no rank on a top-k payload");
                    assert_eq!(envelope.axis_tag, axis.tag(), "{label}: direct axis echo");
                    assert!(envelope.target_is_pcpsit, "{label}: direct PCPSIT echo");
                    assert_eq!(envelope.requested_k, k, "{label}: direct k echo");
                    assert_eq!(
                        envelope.requested_offset, offset,
                        "{label}: direct offset echo"
                    );
                    assert_eq!(
                        envelope.descending, descending,
                        "{label}: direct direction echo"
                    );

                    // Both verifiers accept their own bytes and agree on
                    // everything observable.
                    let (unified_root, unified_entries, unified_skipped) =
                        verify_unified(&unified, &path_query, grove_version);
                    let direct_result = GroveDb::verify_indexed_axis_top_k_paginated(
                        &direct,
                        path,
                        axis,
                        k,
                        offset,
                        descending,
                        grove_version,
                    )
                    .unwrap_or_else(|e| panic!("{label}: direct verify: {e}"));

                    assert_eq!(unified_root, root, "{label}: unified root");
                    assert_eq!(direct_result.root_hash, root, "{label}: direct root");
                    assert_eq!(
                        unified_entries, direct_result.entries,
                        "{label}: verified entries"
                    );
                    assert_eq!(
                        unified_skipped,
                        Some(direct_result.skipped),
                        "{label}: attested skipped"
                    );
                    // Past-the-end pages attest the exhausted population.
                    if offset == 100 {
                        assert_eq!(
                            direct_result.skipped,
                            ENTRIES.len() as u64,
                            "{label}: past-the-end skipped is the population"
                        );
                        assert!(unified_entries.is_empty(), "{label}: past-the-end is empty");
                    }
                }
            }
        }
    }

    // -----------------------------------------------------------------
    // Bounded: axes x directions on a populated PCPSIT
    // -----------------------------------------------------------------

    #[test]
    fn bounded_envelopes_share_the_semantic_core_across_axes_and_directions() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcpsit(&db, grove_version, ENTRIES);
        let root = root_hash(&db, grove_version);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcpsit"];
        let limit: u16 = 10;

        for axis in ALL_AXES {
            let (lo, hi) = bounds_for(axis);
            for descending in [true, false] {
                let label = format!("({axis:?}, descending={descending}, [{lo}, {hi}])");

                let path_query =
                    PathQuery::new_axis_bounded(pcpsit_path(), axis, lo, hi, limit, descending);
                let unified = db
                    .prove_query(&path_query, None, grove_version)
                    .unwrap()
                    .unwrap_or_else(|e| panic!("{label}: unified prove: {e}"));

                // The equivalent direct call lowers the same bounds through
                // the shared prover/verifier-agreement lowering.
                let axis_query = AxisQuery::bounded(axis, lo, hi, limit, descending);
                let lowered =
                    axis_bounded_merk_query(&axis_query).expect("bounded traversal lowers");
                let direct =
                    prove_direct_bounded(&db, path, axis, lowered.clone(), limit, grove_version);

                assert_ne!(
                    unified, direct,
                    "{label}: outer envelopes are documented as different formats"
                );

                let payload = extract_descent_payload(&unified);
                let envelope = decode_range_envelope(&direct);
                assert_semantic_core_eq(
                    &label,
                    &payload,
                    axis.tag(),
                    true,
                    &envelope.other_axes_root_hashes,
                    &envelope.primary_root_hash,
                    &envelope.secondary_proof,
                    &envelope.target_chains,
                );
                assert_eq!(envelope.requested_limit, Some(limit), "{label}: limit echo");
                assert_eq!(envelope.descending, descending, "{label}: direction echo");

                let (unified_root, unified_entries, unified_skipped) =
                    verify_unified(&unified, &path_query, grove_version);
                assert_eq!(
                    unified_skipped, None,
                    "{label}: bounded reads attest no skip count"
                );
                let direct_result =
                    verify_direct_bounded(&direct, path, axis, lowered, limit, grove_version);

                assert_eq!(unified_root, root, "{label}: unified root");
                assert_eq!(direct_result.root_hash, root, "{label}: direct root");
                assert_eq!(
                    unified_entries, direct_result.entries,
                    "{label}: verified entries"
                );
                assert!(
                    !unified_entries.is_empty(),
                    "{label}: the band selects entries"
                );
            }
        }
    }

    /// An in-domain band that selects nothing still proves on both surfaces
    /// with a shared core: the emptiness is authenticated, not assumed.
    #[test]
    fn bounded_envelopes_agree_on_an_empty_selection_window() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcpsit(&db, grove_version, ENTRIES);
        let root = root_hash(&db, grove_version);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcpsit"];
        let (lo, hi, limit) = (1000i128, 2000i128, 10u16);

        let path_query =
            PathQuery::new_axis_bounded(pcpsit_path(), IndexAxis::Sum, lo, hi, limit, false);
        let unified = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("unified prove");
        let axis_query = AxisQuery::bounded(IndexAxis::Sum, lo, hi, limit, false);
        let lowered = axis_bounded_merk_query(&axis_query).expect("lowers");
        let direct = prove_direct_bounded(
            &db,
            path,
            IndexAxis::Sum,
            lowered.clone(),
            limit,
            grove_version,
        );

        let payload = extract_descent_payload(&unified);
        let envelope = decode_range_envelope(&direct);
        assert_semantic_core_eq(
            "empty window",
            &payload,
            IndexAxis::Sum.tag(),
            true,
            &envelope.other_axes_root_hashes,
            &envelope.primary_root_hash,
            &envelope.secondary_proof,
            &envelope.target_chains,
        );
        assert!(payload.target_chains.is_empty(), "no rows selected");

        let (unified_root, unified_entries, _) =
            verify_unified(&unified, &path_query, grove_version);
        let direct_result =
            verify_direct_bounded(&direct, path, IndexAxis::Sum, lowered, limit, grove_version);
        assert_eq!(unified_root, root);
        assert_eq!(direct_result.root_hash, root);
        assert!(unified_entries.is_empty());
        assert_eq!(unified_entries, direct_result.entries);
    }

    // -----------------------------------------------------------------
    // Empty secondaries
    // -----------------------------------------------------------------

    #[test]
    fn envelopes_agree_over_an_empty_secondary_for_both_shapes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcpsit(&db, grove_version, &[]);
        let root = root_hash(&db, grove_version);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcpsit"];

        for axis in ALL_AXES {
            // Paginated: offset 0 and past-the-end-of-nothing.
            for offset in [0u64, 3] {
                let label = format!("empty ({axis:?}, offset={offset})");
                let path_query = PathQuery::new_axis_top_k(pcpsit_path(), axis, 2, offset, true);
                let unified = db
                    .prove_query(&path_query, None, grove_version)
                    .unwrap()
                    .unwrap_or_else(|e| panic!("{label}: unified prove: {e}"));
                let direct = db
                    .prove_indexed_axis_top_k_paginated(
                        path,
                        axis,
                        2,
                        offset,
                        true,
                        None,
                        grove_version,
                    )
                    .unwrap()
                    .unwrap_or_else(|e| panic!("{label}: direct prove: {e}"));

                let payload = extract_descent_payload(&unified);
                let envelope = decode_paginated_envelope(&direct);
                assert_semantic_core_eq(
                    &label,
                    &payload,
                    axis.tag(),
                    true,
                    &envelope.other_axes_root_hashes,
                    &envelope.primary_root_hash,
                    &envelope.secondary_proof,
                    &envelope.target_chains,
                );

                let (unified_root, unified_entries, unified_skipped) =
                    verify_unified(&unified, &path_query, grove_version);
                let direct_result = GroveDb::verify_indexed_axis_top_k_paginated(
                    &direct,
                    path,
                    axis,
                    2,
                    offset,
                    true,
                    grove_version,
                )
                .unwrap_or_else(|e| panic!("{label}: direct verify: {e}"));
                assert_eq!(unified_root, root, "{label}");
                assert_eq!(direct_result.root_hash, root, "{label}");
                assert!(unified_entries.is_empty(), "{label}");
                assert_eq!(unified_entries, direct_result.entries, "{label}");
                assert_eq!(direct_result.skipped, 0, "{label}: nothing to skip");
                assert_eq!(unified_skipped, Some(0), "{label}");
            }

            // Bounded over the empty secondary: THE capability
            // divergence between the surfaces (module doc, divergence
            // 2). The unified prover carries the empty secondary as
            // empty proof bytes, which the verifier resolves to a
            // NULL_HASH secondary root — the parent binding then
            // attests the emptiness. The standalone range prover has no
            // empty-tree shape and refuses outright; this refusal is
            // exactly what platform's drive-abci maps onto its
            // "retry unproved" InvalidArgument for the having surface.
            let (lo, hi) = bounds_for(axis);
            let label = format!("empty ({axis:?}, bounded)");
            let path_query = PathQuery::new_axis_bounded(pcpsit_path(), axis, lo, hi, 10, false);
            let unified = db
                .prove_query(&path_query, None, grove_version)
                .unwrap()
                .unwrap_or_else(|e| panic!("{label}: unified prove: {e}"));
            let payload = extract_descent_payload(&unified);
            assert!(
                payload.secondary_proof.is_empty(),
                "{label}: the unified envelope uses the empty-proof-bytes convention"
            );
            let (unified_root, unified_entries, _) =
                verify_unified(&unified, &path_query, grove_version);
            assert_eq!(unified_root, root, "{label}");
            assert!(unified_entries.is_empty(), "{label}");

            let axis_query = AxisQuery::bounded(axis, lo, hi, 10, false);
            let lowered = axis_bounded_merk_query(&axis_query).expect("lowers");
            let err = prove_direct_bounded_result(&db, path, axis, lowered, 10, grove_version)
                .expect_err(
                    "the standalone range prover refuses an empty secondary; if this starts \
                     succeeding, the module-doc divergence enumeration is stale",
                );
            assert!(
                matches!(&err, Error::CorruptedData(msg)
                    if msg.contains("Cannot create proof for empty tree")),
                "{label}: expected the empty-tree refusal class, got {err:?}"
            );
        }
    }

    // -----------------------------------------------------------------
    // Single-secondary target (PSIT)
    // -----------------------------------------------------------------

    #[test]
    fn psit_envelopes_share_the_semantic_core_on_the_single_secondary_shape() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, ENTRIES);
        let root = root_hash(&db, grove_version);
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];
        let psit_path = vec![TEST_LEAF.to_vec(), b"psit".to_vec()];

        // Paginated.
        let path_query = PathQuery::new_axis_top_k(psit_path.clone(), IndexAxis::Sum, 3, 1, true);
        let unified = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("unified prove");
        let direct = db
            .prove_indexed_sum_top_k_paginated(path, 3, 1, true, None, grove_version)
            .unwrap()
            .expect("direct prove");
        let payload = extract_descent_payload(&unified);
        let envelope = decode_paginated_envelope(&direct);
        assert_semantic_core_eq(
            "PSIT paginated",
            &payload,
            IndexAxis::Sum.tag(),
            false,
            &envelope.other_axes_root_hashes,
            &envelope.primary_root_hash,
            &envelope.secondary_proof,
            &envelope.target_chains,
        );
        assert!(
            envelope.other_axes_root_hashes.is_empty(),
            "a single-secondary target has no other axes"
        );

        let (unified_root, unified_entries, unified_skipped) =
            verify_unified(&unified, &path_query, grove_version);
        let direct_result =
            GroveDb::verify_indexed_sum_top_k_paginated(&direct, path, 3, 1, true, grove_version)
                .expect("direct verify");
        assert_eq!(unified_root, root);
        assert_eq!(direct_result.root_hash, root);
        assert_eq!(unified_entries, direct_result.entries);
        assert_eq!(unified_skipped, Some(direct_result.skipped));

        // Bounded.
        let (lo, hi, limit) = (0i128, 40i128, 10u16);
        let path_query =
            PathQuery::new_axis_bounded(psit_path, IndexAxis::Sum, lo, hi, limit, false);
        let unified = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("unified prove");
        let axis_query = AxisQuery::bounded(IndexAxis::Sum, lo, hi, limit, false);
        let lowered = axis_bounded_merk_query(&axis_query).expect("lowers");
        let direct = db
            .prove_indexed_sum_query(path, lowered.clone(), Some(limit), None, grove_version)
            .unwrap()
            .expect("direct prove");
        let payload = extract_descent_payload(&unified);
        let envelope = decode_range_envelope(&direct);
        assert_semantic_core_eq(
            "PSIT bounded",
            &payload,
            IndexAxis::Sum.tag(),
            false,
            &envelope.other_axes_root_hashes,
            &envelope.primary_root_hash,
            &envelope.secondary_proof,
            &envelope.target_chains,
        );

        let (unified_root, unified_entries, _) =
            verify_unified(&unified, &path_query, grove_version);
        let direct_result =
            GroveDb::verify_indexed_sum_query(&direct, path, lowered, Some(limit), grove_version)
                .expect("direct verify");
        assert_eq!(unified_root, root);
        assert_eq!(direct_result.root_hash, root);
        assert_eq!(unified_entries, direct_result.entries);
    }

    // -----------------------------------------------------------------
    // Mutual acceptance between the verifier entry points
    // -----------------------------------------------------------------

    /// The formats are disjoint by construction, so the cross-check
    /// resolves as mutual rejection: each verifier errors cleanly (no
    /// panic, no false accept) on the other family's bytes. If either
    /// direction ever starts accepting, the divergence documented in the
    /// module doc has materially changed and must be re-examined.
    #[test]
    fn verifier_entry_points_reject_each_others_bytes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcpsit(&db, grove_version, ENTRIES);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcpsit"];

        // Paginated shape, both directions of the cross-feed.
        let path_query = PathQuery::new_axis_top_k(pcpsit_path(), IndexAxis::Sum, 2, 1, true);
        let unified = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("unified prove");
        let direct = db
            .prove_indexed_axis_top_k_paginated(
                path,
                IndexAxis::Sum,
                2,
                1,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("direct prove");

        GroveDb::verify_path_query(&direct, &path_query, grove_version)
            .expect_err("verify_path_query must reject a direct paginated envelope");
        GroveDb::verify_indexed_axis_top_k_paginated(
            &unified,
            path,
            IndexAxis::Sum,
            2,
            1,
            true,
            grove_version,
        )
        .expect_err("the direct paginated verifier must reject a unified envelope");

        // Bounded shape, both directions of the cross-feed.
        let (lo, hi, limit) = (0i128, 40i128, 10u16);
        let path_query =
            PathQuery::new_axis_bounded(pcpsit_path(), IndexAxis::Sum, lo, hi, limit, false);
        let unified = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("unified prove");
        let axis_query = AxisQuery::bounded(IndexAxis::Sum, lo, hi, limit, false);
        let lowered = axis_bounded_merk_query(&axis_query).expect("lowers");
        let direct = prove_direct_bounded(
            &db,
            path,
            IndexAxis::Sum,
            lowered.clone(),
            limit,
            grove_version,
        );

        GroveDb::verify_path_query(&direct, &path_query, grove_version)
            .expect_err("verify_path_query must reject a direct range envelope");
        GroveDb::verify_indexed_sum_query(&unified, path, lowered, Some(limit), grove_version)
            .expect_err("the direct range verifier must reject a unified envelope");
    }
}
