//! Offset-pagination proof tests for the indexed-axis proof primitive
//! (`prove_indexed_axis_top_k_paginated` /
//! `verify_indexed_axis_top_k_paginated`).
//!
//! Every indexed family's secondary carries a hash-bound count
//! aggregate (PCIT: `ProvableCountTree`; PSIT and PCPSIT sum axis:
//! `ProvableCountProvableSumTree`; PCPSIT avg axis:
//! `ProvableCountProvableSumTree`), so "skip the first M entries of the
//! walk, then yield K" is attested by counted subtree commitments
//! instead of enumeration for all of them. These tests pin:
//!
//! - offset 0 yielding the same window as plain top-k,
//! - offsets in the middle of the walk,
//! - offset + k spanning the end of the walk,
//! - offset past the end (an attested proof that the total population
//!   is exactly `skipped` ≤ the requested offset),
//! - single-entry windows ("4th biggest" = offset 3, k 1),
//! - ties straddling the offset boundary (tie-break by original key in
//!   walk direction, in both the skipped prefix and the yielded
//!   window),
//! - both walk directions,
//! - proof-mutation rejection (bit flips either fail verification or
//!   bind a different root hash).

#[cfg(test)]
mod tests {
    use grovedb_element::indexed::IndexAxis;
    use grovedb_version::version::GroveVersion;

    use crate::{
        operations::proof::indexed_axis::AxisEntries,
        tests::{make_test_grovedb, TEST_LEAF},
        Element, GroveDb,
    };

    // -----------------------------------------------------------------
    // Helpers (mirroring indexed_axis_proof_tests.rs builders)
    // -----------------------------------------------------------------

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

    fn build_pcpsit_sum(db: &GroveDb, grove_version: &GroveVersion, entries: &[(&[u8], i64)]) {
        let axes: Vec<(u8, Option<Vec<u8>>)> = vec![(IndexAxis::Sum.tag(), None)];
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

    fn entries_as_sum(entries: &AxisEntries) -> &[(i64, Vec<u8>)] {
        match entries {
            AxisEntries::Sum(v) => v.as_slice(),
            other => panic!("expected sum entries, got {:?}", other),
        }
    }

    fn root_hash(db: &GroveDb, grove_version: &GroveVersion) -> [u8; 32] {
        db.root_hash(None, grove_version).unwrap().expect("root")
    }

    /// The ten-entry fixture used by most tests: distinct sums 10..100.
    /// Ascending walk: a(10) b(20) ... j(100); descending the reverse.
    const TEN: &[(&[u8], i64)] = &[
        (b"a", 10),
        (b"b", 20),
        (b"c", 30),
        (b"d", 40),
        (b"e", 50),
        (b"f", 60),
        (b"g", 70),
        (b"h", 80),
        (b"i", 90),
        (b"j", 100),
    ];

    // -----------------------------------------------------------------
    // offset 0 == top-k
    // -----------------------------------------------------------------

    /// A paginated proof at offset 0 must yield exactly the plain top-k
    /// window (the envelopes differ — range vs paginated — so the
    /// comparison is on the verified entries and root hash, not bytes).
    #[test]
    fn sum_axis_offset_zero_matches_top_k_both_directions() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build_psit(&db, gv, TEN);
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];

        for descending in [false, true] {
            let top_k = db
                .prove_indexed_sum_top_k(path, 3, descending, None, gv)
                .unwrap()
                .expect("prove top-k");
            let top_k_result = GroveDb::verify_indexed_sum_top_k(&top_k, path, 3, descending)
                .expect("verify top-k");

            let paginated = db
                .prove_indexed_sum_top_k_paginated(path, 3, 0, descending, None, gv)
                .unwrap()
                .expect("prove paginated offset 0");
            let paginated_result =
                GroveDb::verify_indexed_sum_top_k_paginated(&paginated, path, 3, 0, descending)
                    .expect("verify paginated offset 0");

            assert_eq!(paginated_result.skipped, 0);
            assert_eq!(
                entries_as_sum(&paginated_result.entries),
                entries_as_sum(&top_k_result.entries),
                "offset 0 (descending={descending}) must equal plain top-k"
            );
            assert_eq!(paginated_result.root_hash, top_k_result.root_hash);
            assert_eq!(paginated_result.root_hash, root_hash(&db, gv));
        }
    }

    // -----------------------------------------------------------------
    // offset in the middle
    // -----------------------------------------------------------------

    #[test]
    fn sum_axis_offset_in_the_middle_both_directions() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build_psit(&db, gv, TEN);
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];

        // Descending walk: j i h | g f e ... — skip 3 take 3 → g f e.
        let proof = db
            .prove_indexed_sum_top_k_paginated(path, 3, 3, true, None, gv)
            .unwrap()
            .expect("prove");
        let result =
            GroveDb::verify_indexed_sum_top_k_paginated(&proof, path, 3, 3, true).expect("verify");
        assert_eq!(result.skipped, 3);
        assert_eq!(
            entries_as_sum(&result.entries),
            &[
                (70i64, b"g".to_vec()),
                (60, b"f".to_vec()),
                (50, b"e".to_vec())
            ]
        );
        assert_eq!(result.root_hash, root_hash(&db, gv));

        // Ascending walk: a b c d | e f ... — skip 4 take 2 → e f.
        let proof = db
            .prove_indexed_sum_top_k_paginated(path, 2, 4, false, None, gv)
            .unwrap()
            .expect("prove");
        let result =
            GroveDb::verify_indexed_sum_top_k_paginated(&proof, path, 2, 4, false).expect("verify");
        assert_eq!(result.skipped, 4);
        assert_eq!(
            entries_as_sum(&result.entries),
            &[(50i64, b"e".to_vec()), (60, b"f".to_vec())]
        );
    }

    // -----------------------------------------------------------------
    // offset + k spanning the end
    // -----------------------------------------------------------------

    /// A window that starts inside the walk but extends past its end
    /// yields the tail (< k entries) with the full offset attested.
    #[test]
    fn sum_axis_offset_plus_k_spanning_the_end_yields_short_page() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build_psit(&db, gv, TEN);
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];

        // Descending: skip 8 (j..c), ask for 5 → only b, a remain.
        let proof = db
            .prove_indexed_sum_top_k_paginated(path, 5, 8, true, None, gv)
            .unwrap()
            .expect("prove");
        let result =
            GroveDb::verify_indexed_sum_top_k_paginated(&proof, path, 5, 8, true).expect("verify");
        assert_eq!(result.skipped, 8);
        assert_eq!(
            entries_as_sum(&result.entries),
            &[(20i64, b"b".to_vec()), (10, b"a".to_vec())],
            "the page is the walk's tail, shorter than k"
        );
        assert_eq!(result.root_hash, root_hash(&db, gv));
    }

    // -----------------------------------------------------------------
    // offset past the end: proof that total count <= M
    // -----------------------------------------------------------------

    /// An offset past the end is provable: the counted commitments
    /// cover the entire walk, so `skipped` equals the total population
    /// and the empty page is a proof that the population is ≤ the
    /// requested offset. This is the "prove total count ≤ M" shape.
    #[test]
    fn sum_axis_offset_past_end_attests_total_population() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build_psit(&db, gv, TEN);
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];

        for (offset, descending) in [(11u64, false), (11, true), (1_000_000, false)] {
            let proof = db
                .prove_indexed_sum_top_k_paginated(path, 3, offset, descending, None, gv)
                .unwrap()
                .expect("prove offset past end");
            let result =
                GroveDb::verify_indexed_sum_top_k_paginated(&proof, path, 3, offset, descending)
                    .expect("verify offset past end");
            assert_eq!(
                result.skipped, 10,
                "the attested skipped count is the total population"
            );
            assert!(result.entries.is_empty());
            assert!(result.skipped < offset);
            assert_eq!(result.root_hash, root_hash(&db, gv));
        }

        // Offset exactly at the population boundary: everything is
        // skipped, the page is empty, and skipped == offset — so this
        // shape alone does NOT prove the population equals the offset
        // (it proves ≥). skipped < offset is the strict "population ==
        // skipped" witness, tested above.
        let proof = db
            .prove_indexed_sum_top_k_paginated(path, 3, 10, false, None, gv)
            .unwrap()
            .expect("prove offset == population");
        let result = GroveDb::verify_indexed_sum_top_k_paginated(&proof, path, 3, 10, false)
            .expect("verify offset == population");
        assert_eq!(result.skipped, 10);
        assert!(result.entries.is_empty());
    }

    /// Empty tree: any offset yields skipped = 0, empty page — a proof
    /// that the population is 0.
    #[test]
    fn sum_axis_offset_on_empty_tree_attests_zero_population() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build_psit(&db, gv, &[]);
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];

        let proof = db
            .prove_indexed_sum_top_k_paginated(path, 3, 5, false, None, gv)
            .unwrap()
            .expect("prove on empty");
        let result = GroveDb::verify_indexed_sum_top_k_paginated(&proof, path, 3, 5, false)
            .expect("verify on empty");
        assert_eq!(result.skipped, 0);
        assert!(result.entries.is_empty());
        assert_eq!(result.root_hash, root_hash(&db, gv));
    }

    // -----------------------------------------------------------------
    // single-entry windows
    // -----------------------------------------------------------------

    /// "The 4th biggest" = descending walk, offset 3, k 1.
    #[test]
    fn sum_axis_fourth_biggest_is_offset_three_k_one() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build_psit(&db, gv, TEN);
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];

        let proof = db
            .prove_indexed_sum_top_k_paginated(path, 1, 3, true, None, gv)
            .unwrap()
            .expect("prove 4th biggest");
        let result = GroveDb::verify_indexed_sum_top_k_paginated(&proof, path, 1, 3, true)
            .expect("verify 4th biggest");
        assert_eq!(result.skipped, 3);
        assert_eq!(
            entries_as_sum(&result.entries),
            &[(70i64, b"g".to_vec())],
            "rank 4 descending of sums 10..100 is g(70)"
        );

        // Every rank is individually addressable: walk them all.
        let expect_desc: &[(i64, &[u8])] = &[
            (100, b"j"),
            (90, b"i"),
            (80, b"h"),
            (70, b"g"),
            (60, b"f"),
            (50, b"e"),
            (40, b"d"),
            (30, b"c"),
            (20, b"b"),
            (10, b"a"),
        ];
        for (rank_zero_based, (sum, key)) in expect_desc.iter().enumerate() {
            let proof = db
                .prove_indexed_sum_top_k_paginated(path, 1, rank_zero_based as u64, true, None, gv)
                .unwrap()
                .expect("prove rank window");
            let result = GroveDb::verify_indexed_sum_top_k_paginated(
                &proof,
                path,
                1,
                rank_zero_based as u64,
                true,
            )
            .expect("verify rank window");
            assert_eq!(result.skipped, rank_zero_based as u64);
            assert_eq!(entries_as_sum(&result.entries), &[(*sum, key.to_vec())]);
        }
    }

    // -----------------------------------------------------------------
    // ties straddling the offset boundary
    // -----------------------------------------------------------------

    /// Equal sums tie-break by original key in walk direction — in both
    /// the skipped prefix and the yielded window. The offset boundary
    /// falls INSIDE the tie group, pinning that the split is
    /// deterministic and attested.
    #[test]
    fn sum_axis_ties_straddling_the_offset_boundary() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        // Five entries share sum 50; two below, two above.
        build_psit(
            &db,
            gv,
            &[
                (b"lo1", 10),
                (b"lo2", 20),
                (b"t_a", 50),
                (b"t_b", 50),
                (b"t_c", 50),
                (b"t_d", 50),
                (b"t_e", 50),
                (b"hi1", 90),
                (b"hi2", 100),
            ],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];

        // Ascending walk: lo1 lo2 t_a t_b t_c t_d t_e hi1 hi2 (ties in
        // ascending lex order of key). Offset 4 lands mid-tie: skip
        // lo1 lo2 t_a t_b → yield t_c t_d t_e.
        let proof = db
            .prove_indexed_sum_top_k_paginated(path, 3, 4, false, None, gv)
            .unwrap()
            .expect("prove mid-tie ascending");
        let result = GroveDb::verify_indexed_sum_top_k_paginated(&proof, path, 3, 4, false)
            .expect("verify mid-tie ascending");
        assert_eq!(result.skipped, 4);
        assert_eq!(
            entries_as_sum(&result.entries),
            &[
                (50i64, b"t_c".to_vec()),
                (50, b"t_d".to_vec()),
                (50, b"t_e".to_vec())
            ]
        );

        // Descending walk: hi2 hi1 t_e t_d t_c t_b t_a lo2 lo1 (ties in
        // DESCENDING lex order of key). Offset 3 lands mid-tie: skip
        // hi2 hi1 t_e → yield t_d t_c t_b.
        let proof = db
            .prove_indexed_sum_top_k_paginated(path, 3, 3, true, None, gv)
            .unwrap()
            .expect("prove mid-tie descending");
        let result = GroveDb::verify_indexed_sum_top_k_paginated(&proof, path, 3, 3, true)
            .expect("verify mid-tie descending");
        assert_eq!(result.skipped, 3);
        assert_eq!(
            entries_as_sum(&result.entries),
            &[
                (50i64, b"t_d".to_vec()),
                (50, b"t_c".to_vec()),
                (50, b"t_b".to_vec())
            ]
        );

        // The two directions' windows at the same offset partition the
        // tie group consistently: ascending offset 4 k 3 and descending
        // offset 3 k 3 both contain t_c and t_d — the walk order is a
        // total order, not a per-proof choice.
    }

    // -----------------------------------------------------------------
    // PCPSIT sum axis (multi-axis family, same primitive)
    // -----------------------------------------------------------------

    /// The PCPSIT sum axis pages through the same count-offset
    /// primitive, including mid-tie offsets and offset-past-end.
    #[test]
    fn pcpsit_sum_axis_offset_pagination_round_trips() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build_pcpsit_sum(
            &db,
            gv,
            &[
                (b"a", 5),
                (b"b", 5),
                (b"c", 5),
                (b"d", 40),
                (b"e", 50),
                (b"f", 60),
            ],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"pcpsit"];

        // Ascending, offset 1 lands mid-tie (skip a → yield b c d).
        let proof = db
            .prove_indexed_sum_top_k_paginated(path, 3, 1, false, None, gv)
            .unwrap()
            .expect("prove");
        let result =
            GroveDb::verify_indexed_sum_top_k_paginated(&proof, path, 3, 1, false).expect("verify");
        assert_eq!(result.skipped, 1);
        assert_eq!(
            entries_as_sum(&result.entries),
            &[
                (5i64, b"b".to_vec()),
                (5, b"c".to_vec()),
                (40, b"d".to_vec())
            ]
        );
        assert_eq!(result.root_hash, root_hash(&db, gv));

        // Offset past end.
        let proof = db
            .prove_indexed_sum_top_k_paginated(path, 3, 99, true, None, gv)
            .unwrap()
            .expect("prove past end");
        let result = GroveDb::verify_indexed_sum_top_k_paginated(&proof, path, 3, 99, true)
            .expect("verify past end");
        assert_eq!(result.skipped, 6, "total population attested");
        assert!(result.entries.is_empty());
    }

    // -----------------------------------------------------------------
    // proof mutation rejection
    // -----------------------------------------------------------------

    /// Flipping any single bit of a paginated proof must either fail
    /// verification outright or reconstruct a root hash that no longer
    /// matches the database's — a mutated proof can never verify
    /// against the authentic root with different contents.
    #[test]
    fn sum_axis_paginated_proof_bit_flips_are_rejected_or_rebound() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build_psit(&db, gv, TEN);
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];
        let expected_root = root_hash(&db, gv);

        let proof = db
            .prove_indexed_sum_top_k_paginated(path, 3, 3, true, None, gv)
            .unwrap()
            .expect("prove");
        let baseline = GroveDb::verify_indexed_sum_top_k_paginated(&proof, path, 3, 3, true)
            .expect("baseline verifies");
        assert_eq!(baseline.root_hash, expected_root);
        let baseline_entries = entries_as_sum(&baseline.entries).to_vec();

        let mut accepted_with_same_meaning = 0usize;
        for byte_idx in 0..proof.len() {
            for bit in 0..8u8 {
                let mut mutated = proof.clone();
                mutated[byte_idx] ^= 1 << bit;
                match GroveDb::verify_indexed_sum_top_k_paginated(&mutated, path, 3, 3, true) {
                    Err(_) => {}
                    Ok(result) => {
                        if result.root_hash == expected_root
                            && result.skipped == baseline.skipped
                            && entries_as_sum(&result.entries) == baseline_entries.as_slice()
                        {
                            // A flip that decodes to the identical verified
                            // meaning (e.g. inside bincode slack) is not a
                            // forgery. Anything else under the authentic
                            // root would be.
                            accepted_with_same_meaning += 1;
                        } else {
                            assert_ne!(
                                result.root_hash, expected_root,
                                "bit flip at byte {byte_idx} bit {bit} verified DIFFERENT \
                                 content under the authentic root hash"
                            );
                        }
                    }
                }
            }
        }
        // Sanity: the loop exercised real mutations (the proof is not
        // somehow all slack bytes).
        assert!(
            accepted_with_same_meaning < proof.len() * 8,
            "every mutation decoded identically — mutation harness is broken"
        );
    }

    /// Truncated and garbage-extended proofs are rejected.
    #[test]
    fn sum_axis_paginated_proof_truncation_and_garbage_are_rejected() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build_psit(&db, gv, TEN);
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];

        let proof = db
            .prove_indexed_sum_top_k_paginated(path, 2, 2, false, None, gv)
            .unwrap()
            .expect("prove");

        let truncated = &proof[..proof.len() - 1];
        assert!(
            GroveDb::verify_indexed_sum_top_k_paginated(truncated, path, 2, 2, false).is_err(),
            "truncated proof must not verify"
        );

        let mut extended = proof.clone();
        extended.extend_from_slice(b"garbage");
        assert!(
            GroveDb::verify_indexed_sum_top_k_paginated(&extended, path, 2, 2, false).is_err(),
            "garbage-extended proof must not verify"
        );
    }

    /// Parameter mismatches (k / offset / direction) are rejected even
    /// against an honest proof.
    #[test]
    fn sum_axis_paginated_parameter_mismatches_are_rejected() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build_psit(&db, gv, TEN);
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];

        let proof = db
            .prove_indexed_sum_top_k_paginated(path, 3, 2, true, None, gv)
            .unwrap()
            .expect("prove");

        assert!(
            GroveDb::verify_indexed_sum_top_k_paginated(&proof, path, 4, 2, true).is_err(),
            "wrong k must be rejected"
        );
        assert!(
            GroveDb::verify_indexed_sum_top_k_paginated(&proof, path, 3, 3, true).is_err(),
            "wrong offset must be rejected"
        );
        assert!(
            GroveDb::verify_indexed_sum_top_k_paginated(&proof, path, 3, 2, false).is_err(),
            "wrong direction must be rejected"
        );
    }

    // -----------------------------------------------------------------
    // rank-of-key
    // -----------------------------------------------------------------

    /// `prove_indexed_axis_rank_of_key` proves "exactly R entries come
    /// strictly before X in the walk" and binds it to X: the proof is a
    /// paginated window (offset = R, k = 1) whose yielded entry is X.
    #[test]
    fn sum_axis_rank_of_key_round_trips_for_every_key_both_directions() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build_psit(&db, gv, TEN);
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];
        let expected_root = root_hash(&db, gv);

        // Descending: j has rank 0, i rank 1, ... a rank 9. Ascending
        // is the reverse.
        let desc_order: &[&[u8]] = &[b"j", b"i", b"h", b"g", b"f", b"e", b"d", b"c", b"b", b"a"];
        for descending in [true, false] {
            for (expected_rank, key) in desc_order.iter().enumerate() {
                let expected_rank = if descending {
                    expected_rank as u64
                } else {
                    (desc_order.len() - 1 - expected_rank) as u64
                };
                let (proof, rank) = db
                    .prove_indexed_axis_rank_of_key(path, IndexAxis::Sum, key, descending, None, gv)
                    .unwrap()
                    .expect("prove rank of key");
                assert_eq!(rank, expected_rank, "prover-reported rank");
                let result = GroveDb::verify_indexed_axis_rank_of_key(
                    &proof,
                    path,
                    IndexAxis::Sum,
                    key,
                    expected_rank,
                    descending,
                )
                .expect("verify rank of key");
                assert_eq!(result.root_hash, expected_root);
                assert_eq!(result.skipped, expected_rank);
            }
        }
    }

    /// Rank inside a tie group follows the walk's total order (tie-break
    /// by original key in walk direction).
    #[test]
    fn sum_axis_rank_of_key_inside_a_tie_group() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build_psit(
            &db,
            gv,
            &[
                (b"lo", 10),
                (b"t_a", 50),
                (b"t_b", 50),
                (b"t_c", 50),
                (b"hi", 90),
            ],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];

        // Ascending walk: lo t_a t_b t_c hi → t_b has rank 2.
        let (proof, rank) = db
            .prove_indexed_axis_rank_of_key(path, IndexAxis::Sum, b"t_b", false, None, gv)
            .unwrap()
            .expect("prove");
        assert_eq!(rank, 2);
        GroveDb::verify_indexed_axis_rank_of_key(&proof, path, IndexAxis::Sum, b"t_b", 2, false)
            .expect("verify ascending mid-tie rank");

        // Descending walk: hi t_c t_b t_a lo → t_b has rank 2 there too
        // (symmetric fixture), t_c has rank 1.
        let (proof, rank) = db
            .prove_indexed_axis_rank_of_key(path, IndexAxis::Sum, b"t_c", true, None, gv)
            .unwrap()
            .expect("prove");
        assert_eq!(rank, 1);
        GroveDb::verify_indexed_axis_rank_of_key(&proof, path, IndexAxis::Sum, b"t_c", 1, true)
            .expect("verify descending mid-tie rank");
    }

    /// A rank proof only verifies for the exact (key, rank) pair it was
    /// generated for; a wrong rank or a different key is rejected.
    #[test]
    fn sum_axis_rank_of_key_wrong_claims_are_rejected() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build_psit(&db, gv, TEN);
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];

        // g(70) has descending rank 3.
        let (proof, rank) = db
            .prove_indexed_axis_rank_of_key(path, IndexAxis::Sum, b"g", true, None, gv)
            .unwrap()
            .expect("prove");
        assert_eq!(rank, 3);

        assert!(
            GroveDb::verify_indexed_axis_rank_of_key(&proof, path, IndexAxis::Sum, b"g", 4, true)
                .is_err(),
            "a different rank claim must be rejected (offset echo mismatch)"
        );
        assert!(
            GroveDb::verify_indexed_axis_rank_of_key(&proof, path, IndexAxis::Sum, b"h", 3, true)
                .is_err(),
            "a different key claim must be rejected (the entry at rank 3 is g)"
        );
        assert!(
            GroveDb::verify_indexed_axis_rank_of_key(&proof, path, IndexAxis::Sum, b"g", 3, false)
                .is_err(),
            "a different direction must be rejected"
        );
    }

    /// Proving the rank of a key that is not in the primary fails at
    /// prove time.
    #[test]
    fn sum_axis_rank_of_absent_key_fails_to_prove() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build_psit(&db, gv, TEN);
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];

        let err = db
            .prove_indexed_axis_rank_of_key(path, IndexAxis::Sum, b"nope", true, None, gv)
            .unwrap()
            .expect_err("rank of an absent key is unprovable");
        assert!(
            matches!(err, crate::Error::PathKeyNotFound(_)),
            "an absent key is a not-found error, not corruption: {err:?}"
        );
    }

    /// Rank proving rejects invalid targets before touching the
    /// secondary: the root path has no indexed element, and a
    /// non-indexed tree is not an indexed primary.
    #[test]
    fn rank_of_key_rejects_root_path_and_non_indexed_targets() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build_psit(&db, gv, TEN);

        let root: &[&[u8]] = &[];
        assert!(
            db.prove_indexed_axis_rank_of_key(root, IndexAxis::Sum, b"a", false, None, gv)
                .unwrap()
                .is_err(),
            "rank at the root path is rejected"
        );

        // TEST_LEAF itself is a plain tree, not an indexed primary.
        let plain: &[&[u8]] = &[TEST_LEAF];
        assert!(
            db.prove_indexed_axis_rank_of_key(plain, IndexAxis::Sum, b"psit", false, None, gv)
                .unwrap()
                .is_err(),
            "rank against a non-indexed tree is rejected"
        );
    }

    /// The rank verifier's own checks, beyond the paginated echoes:
    /// a claimed rank past the walk's end fails the
    /// `skipped == expected_rank` requirement, and a rank exactly at
    /// the population boundary (empty window, fully satisfied skip)
    /// fails the single-yielded-entry requirement — no key sits at a
    /// rank equal to the population.
    #[test]
    fn rank_verifier_rejects_ranks_at_or_past_the_population() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build_psit(&db, gv, TEN);
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];

        // Rank 99 on a 10-entry walk: the paginated proof itself is
        // honest (skipped = 10 < 99, empty page), so the paginated
        // verifier accepts it — the RANK verifier must reject it.
        let proof = db
            .prove_indexed_sum_top_k_paginated(path, 1, 99, false, None, gv)
            .unwrap()
            .expect("prove offset past end");
        GroveDb::verify_indexed_sum_top_k_paginated(&proof, path, 1, 99, false)
            .expect("paginated shape verifies");
        assert!(
            GroveDb::verify_indexed_axis_rank_of_key(&proof, path, IndexAxis::Sum, b"a", 99, false)
                .is_err(),
            "a rank claim past the population must be rejected (skipped < rank)"
        );

        // Rank 10 == population: skip fully satisfied but the window is
        // empty, so there is no entry to bind the key to.
        let proof = db
            .prove_indexed_sum_top_k_paginated(path, 1, 10, false, None, gv)
            .unwrap()
            .expect("prove offset == population");
        assert!(
            GroveDb::verify_indexed_axis_rank_of_key(&proof, path, IndexAxis::Sum, b"a", 10, false)
                .is_err(),
            "a rank claim equal to the population must be rejected (no yielded entry)"
        );
    }

    /// Trailing bytes after the envelope are rejected on the range
    /// (top-k) and aggregate envelope decoders too, mirroring the
    /// paginated case tested above — the three shapes share the
    /// anti-malleability rule.
    #[test]
    fn range_and_aggregate_envelopes_reject_trailing_bytes() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build_psit(&db, gv, TEN);
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];

        let mut top_k = db
            .prove_indexed_sum_top_k(path, 3, true, None, gv)
            .unwrap()
            .expect("prove top-k");
        GroveDb::verify_indexed_sum_top_k(&top_k, path, 3, true).expect("clean top-k verifies");
        top_k.push(0);
        assert!(
            GroveDb::verify_indexed_sum_top_k(&top_k, path, 3, true).is_err(),
            "trailing byte after the range envelope must be rejected"
        );

        let mut aggregate = db
            .prove_indexed_sum_range_aggregate(path, 0, 100, None, gv)
            .unwrap()
            .expect("prove aggregate");
        GroveDb::verify_indexed_sum_range_aggregate(&aggregate, path, 0, 100)
            .expect("clean aggregate verifies");
        aggregate.push(0);
        assert!(
            GroveDb::verify_indexed_sum_range_aggregate(&aggregate, path, 0, 100).is_err(),
            "trailing byte after the aggregate envelope must be rejected"
        );
    }
}
