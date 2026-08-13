//! Cost and equality pins for the unproved paginated ranked reads.
//!
//! The offset skip in `indexed_<axis>_top_k_paginated` must be *counted* —
//! one descent through the secondary merk, skipping whole subtrees via link
//! aggregate counts — rather than *linear* (one storage-iterator step per
//! skipped entry). Platform-side executors discard `CostContext`, so the
//! assertion that the skip is counted has to live here, where
//! `OperationCost` is visible: seek counts at a deep offset are bounded by
//! the tree depth, not by the offset.
//!
//! The equality tests pin that the counted path returns byte-identical
//! entries in identical order to the storage-iterator order, using the
//! unchanged `indexed_<axis>_top_k` iterator path as the oracle.

#[cfg(test)]
mod tests {
    use grovedb_costs::CostContext;
    use grovedb_version::version::GroveVersion;

    use crate::{
        batch::QualifiedGroveDbOp,
        operations::proof::indexed_axis::AxisEntries,
        tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
        Element, GroveDb,
    };

    /// Rows in the large tie-heavy fixture. Big enough that a linear skip
    /// (≈ one seek per skipped row) is orders of magnitude over the
    /// depth-bounded budget the cost test allows.
    const N: u64 = 600;
    const K: u16 = 5;

    /// AVL depth bound for a tree of `n` keys: 1.44·log2(n + 2).
    fn avl_depth_bound(n: u64) -> u32 {
        (1.44 * ((n + 2) as f64).log2()).ceil() as u32
    }

    /// PCIT with `n` Item children inserted via chunked batches. Every
    /// Item child derives count = 1, so the count axis is maximally
    /// tie-heavy and the secondary order is exactly the item-key tiebreak.
    fn make_pcit_with_rows(n: u64, grove_version: &GroveVersion) -> TempGroveDb {
        let db = make_test_grovedb(grove_version);
        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"cidx".to_vec(),
                Element::empty_provable_count_indexed_tree(),
            )],
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PCIT");
        const CHUNK: u64 = 50_000;
        let mut next = 0u64;
        while next < n {
            let end = (next + CHUNK).min(n);
            let ops = (next..end)
                .map(|i| {
                    QualifiedGroveDbOp::insert_or_replace_op(
                        vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                        format!("k{i:07}").into_bytes(),
                        Element::new_item(vec![]),
                    )
                })
                .collect();
            db.apply_batch(ops, None, None, grove_version)
                .unwrap()
                .expect("populate PCIT batch chunk");
            next = end;
        }
        db
    }

    fn make_pcit_with_n_tied_rows(grove_version: &GroveVersion) -> TempGroveDb {
        make_pcit_with_rows(N, grove_version)
    }

    /// PCIT with count-tree children whose derived counts contain both
    /// distinct values and ties (3×3 and 2×9), so the axis-value ordering
    /// and the item-key tiebreak are both exercised.
    fn make_pcit_with_mixed_counts(grove_version: &GroveVersion) -> (TempGroveDb, usize) {
        let dataset: &[(&[u8], u64)] = &[
            (b"a", 5),
            (b"b", 12),
            (b"c", 1),
            (b"d", 7),
            (b"e", 20),
            (b"f", 3),
            (b"g", 3),
            (b"h", 3),
            (b"i", 9),
            (b"j", 9),
        ];
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
        for (key, count) in dataset {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                key,
                Element::empty_count_tree(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert count-tree child");
            for i in 0..*count {
                db.insert(
                    [TEST_LEAF, b"cidx", key].as_ref(),
                    &i.to_be_bytes(),
                    Element::new_item(vec![]),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("populate count-tree child");
            }
        }
        (db, dataset.len())
    }

    /// PCPSIT (all three axes) with count-sum-tree children whose derived
    /// sums include a tie group (10 appears three times), so the sum and
    /// avg axes exercise the item-key tiebreak too.
    fn make_pcpsit_with_mixed_sums(grove_version: &GroveVersion) -> (TempGroveDb, usize) {
        let dataset: &[(&[u8], u64, i64)] = &[
            (b"a", 2, 10),
            (b"b", 4, 100),
            (b"c", 5, -25),
            (b"d", 1, 0),
            (b"e", 3, 9),
            (b"f", 1, 10),
            (b"g", 2, 10),
        ];
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            Element::empty_provable_count_provable_sum_indexed_tree(vec![
                (0u8, None),
                (1u8, None),
                (2u8, None),
            ])
            .expect("canonical axes"),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert PCPSIT");
        for (key, count, sum) in dataset {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                key,
                Element::empty_count_sum_tree(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert count-sum-tree child");
            for i in 0..*count {
                db.insert(
                    [TEST_LEAF, b"pcpsit", key].as_ref(),
                    &i.to_be_bytes(),
                    Element::new_sum_item(if i == 0 { *sum } else { 0 }),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("populate count-sum-tree child");
            }
        }
        (db, dataset.len())
    }

    // -----------------------------------------------------------------
    // The counted-skip assertion: offset cost is bounded by tree depth,
    // never by the offset itself.
    // -----------------------------------------------------------------

    #[test]
    fn paginated_offset_skip_is_counted_not_linear() {
        let grove_version = GroveVersion::latest();
        let db = make_pcit_with_n_tied_rows(grove_version);
        let path = [TEST_LEAF, b"cidx"];

        for descending in [false, true] {
            let CostContext {
                value: page0,
                cost: cost_0,
            } = db.indexed_count_top_k_paginated(
                path.as_ref(),
                K,
                0,
                descending,
                None,
                grove_version,
            );
            let page0 = page0.expect("offset 0");
            assert_eq!(page0.entries.len(), K as usize);
            assert_eq!(page0.skipped, 0, "offset 0 skips nothing");

            let far_offset = N - K as u64;
            let CostContext {
                value: page_far,
                cost: cost_far,
            } = db.indexed_count_top_k_paginated(
                path.as_ref(),
                K,
                far_offset,
                descending,
                None,
                grove_version,
            );
            let page_far = page_far.expect("deep offset");
            assert_eq!(
                page_far.entries.len(),
                K as usize,
                "last full page must exist"
            );
            assert_eq!(
                page_far.skipped, far_offset,
                "true skipped at a deep offset"
            );

            let depth_bound = avl_depth_bound(N);

            // A counted skip pays at most one root-to-position descent (plus
            // the k-collect walk) for the offset — never per-skipped-entry
            // work. A linear skip pays ≈ one seek per skipped row (~595
            // here), which blows this budget by an order of magnitude.
            assert!(
                cost_far.seek_count <= cost_0.seek_count + 3 * depth_bound,
                "offset skip is walking entries (descending={descending}): seek_count {} at \
                 offset {} vs {} at offset 0 (depth bound {})",
                cost_far.seek_count,
                far_offset,
                cost_0.seek_count,
                depth_bound,
            );
            // ~300 B per fetched node; 512 B/level keeps 2x headroom while
            // staying an order of magnitude under the skipped region's bytes.
            assert!(
                cost_far.storage_loaded_bytes
                    <= cost_0.storage_loaded_bytes + u64::from(depth_bound) * 512,
                "offset skip is loading the skipped region (descending={descending}): {} bytes \
                 at offset {} vs {} at offset 0",
                cost_far.storage_loaded_bytes,
                far_offset,
                cost_0.storage_loaded_bytes,
            );

            // Past the end: the root aggregate (resident from the open)
            // alone proves the offset exceeds the population — no descent at
            // all. Offset 0 costs open + iterator seek + K collect steps, so
            // "no descent" pins as: past-end + the collect work still fits
            // inside the offset-0 budget.
            let CostContext {
                value: past,
                cost: cost_past,
            } = db.indexed_count_top_k_paginated(
                path.as_ref(),
                K,
                N + 100,
                descending,
                None,
                grove_version,
            );
            let past = past.expect("past-end offset");
            assert!(
                past.entries.is_empty(),
                "past-end offset must return an empty page"
            );
            assert_eq!(
                past.skipped, N,
                "past-end must report the true population, not echo the request"
            );
            assert!(
                cost_past.seek_count + u32::from(K) <= cost_0.seek_count,
                "past-end offset must not descend (descending={descending}): seek_count {} vs \
                 {} at offset 0 (k = {K})",
                cost_past.seek_count,
                cost_0.seek_count,
            );
        }
    }

    /// An empty secondary has no root to descend into: any offset must
    /// return an empty page without erroring, on both the iterator path
    /// (offset 0) and the counted path (offset > 0).
    #[test]
    fn paginated_on_empty_secondary_returns_empty_at_any_offset() {
        let grove_version = GroveVersion::latest();
        let db = make_pcit_with_rows(0, grove_version);
        let path = [TEST_LEAF, b"cidx"];

        for descending in [false, true] {
            for offset in [0u64, 1, 1_000_000] {
                let page = db
                    .indexed_count_top_k_paginated(
                        path.as_ref(),
                        3,
                        offset,
                        descending,
                        None,
                        grove_version,
                    )
                    .unwrap()
                    .expect("empty secondary must serve an empty page");
                assert!(
                    page.entries.is_empty(),
                    "offset={offset} descending={descending} must be empty"
                );
                assert_eq!(page.skipped, 0, "an empty secondary has nothing to skip");
            }
        }
    }

    // -----------------------------------------------------------------
    // Offset-0 non-regression: the common shape stays on the iterator
    // path, at exactly the plain top-k cost.
    // -----------------------------------------------------------------

    #[test]
    fn paginated_offset_zero_costs_exactly_plain_top_k() {
        let grove_version = GroveVersion::latest();
        let db = make_pcit_with_n_tied_rows(grove_version);
        let path = [TEST_LEAF, b"cidx"];

        for descending in [false, true] {
            let CostContext {
                value: plain,
                cost: cost_plain,
            } = db.indexed_count_top_k(path.as_ref(), K, descending, None, grove_version);
            let CostContext {
                value: paginated,
                cost: cost_paginated,
            } = db.indexed_count_top_k_paginated(
                path.as_ref(),
                K,
                0,
                descending,
                None,
                grove_version,
            );
            let paginated = paginated.expect("paginated offset 0");
            assert_eq!(
                plain.expect("plain top-k"),
                paginated.entries,
                "offset 0 must return exactly the top-k page (descending={descending})"
            );
            assert_eq!(paginated.skipped, 0);
            assert_eq!(
                cost_plain.seek_count, cost_paginated.seek_count,
                "offset 0 must stay on the iterator path: seek_count diverged \
                 (descending={descending})"
            );
            assert_eq!(
                cost_plain.storage_loaded_bytes, cost_paginated.storage_loaded_bytes,
                "offset 0 must stay on the iterator path: loaded bytes diverged \
                 (descending={descending})"
            );
        }
    }

    // -----------------------------------------------------------------
    // Equality grids: the counted path must return byte-identical pages
    // in identical order to the iterator order. Oracle: the unchanged
    // `indexed_<axis>_top_k` iterator path, sliced.
    // -----------------------------------------------------------------

    #[test]
    fn counted_paginated_matches_iterator_oracle_count_axis() {
        let grove_version = GroveVersion::latest();
        let (db, population) = make_pcit_with_mixed_counts(grove_version);
        let path = [TEST_LEAF, b"cidx"];

        for descending in [false, true] {
            let full = db
                .indexed_count_top_k(
                    path.as_ref(),
                    population as u16,
                    descending,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("full ordered scan (oracle)");
            assert_eq!(full.len(), population);

            for offset in [0u64, 1, 2, 3, 4, 5, 8, 9, 10, 11, 15] {
                for k in [0u16, 1, 3, population as u16] {
                    let page = db
                        .indexed_count_top_k_paginated(
                            path.as_ref(),
                            k,
                            offset,
                            descending,
                            None,
                            grove_version,
                        )
                        .unwrap()
                        .expect("paginated page");
                    let start = (offset as usize).min(population);
                    let end = (start + k as usize).min(population);
                    assert_eq!(
                        page.entries,
                        full[start..end],
                        "count axis page mismatch at offset={offset} k={k} \
                         descending={descending}"
                    );
                    assert_eq!(
                        page.skipped,
                        offset.min(population as u64),
                        "true skipped at offset={offset} k={k}"
                    );
                }
            }
        }
    }

    #[test]
    fn counted_paginated_matches_iterator_oracle_sum_and_avg_axes() {
        let grove_version = GroveVersion::latest();
        let (db, population) = make_pcpsit_with_mixed_sums(grove_version);
        let path = [TEST_LEAF, b"pcpsit"];

        for descending in [false, true] {
            let full_sum = db
                .indexed_sum_top_k(
                    path.as_ref(),
                    population as u16,
                    descending,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("full sum scan (oracle)");
            let full_avg = db
                .indexed_avg_top_k(
                    path.as_ref(),
                    population as u16,
                    descending,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("full avg scan (oracle)");
            assert_eq!(full_sum.len(), population);
            assert_eq!(full_avg.len(), population);

            for offset in [0u64, 1, 2, 3, 5, 6, 7, 9] {
                for k in [0u16, 1, 2, population as u16] {
                    let start = (offset as usize).min(population);
                    let end = (start + k as usize).min(population);

                    let sum_page = db
                        .indexed_sum_top_k_paginated(
                            path.as_ref(),
                            k,
                            offset,
                            descending,
                            None,
                            grove_version,
                        )
                        .unwrap()
                        .expect("sum page");
                    assert_eq!(
                        sum_page.entries,
                        full_sum[start..end],
                        "sum axis page mismatch at offset={offset} k={k} \
                         descending={descending}"
                    );
                    assert_eq!(sum_page.skipped, (offset).min(population as u64));

                    let avg_page = db
                        .indexed_avg_top_k_paginated(
                            path.as_ref(),
                            k,
                            offset,
                            descending,
                            None,
                            grove_version,
                        )
                        .unwrap()
                        .expect("avg page");
                    assert_eq!(
                        avg_page.entries,
                        full_avg[start..end],
                        "avg axis page mismatch at offset={offset} k={k} \
                         descending={descending}"
                    );
                    assert_eq!(avg_page.skipped, (offset).min(population as u64));
                }
            }
        }
    }

    #[test]
    fn counted_paginated_matches_iterator_oracle_across_subtree_boundaries() {
        let grove_version = GroveVersion::latest();
        let db = make_pcit_with_n_tied_rows(grove_version);
        let path = [TEST_LEAF, b"cidx"];

        // Offsets chosen to land inside, at, and across internal subtree
        // boundaries of a ~600-key AVL tree, plus the exact-population and
        // past-end edges.
        for descending in [false, true] {
            let full = db
                .indexed_count_top_k(path.as_ref(), N as u16, descending, None, grove_version)
                .unwrap()
                .expect("full ordered scan (oracle)");
            assert_eq!(full.len(), N as usize);

            for offset in [1u64, 7, 63, 64, 65, 250, 511, N - 6, N - 1, N, N + 17] {
                let page = db
                    .indexed_count_top_k_paginated(
                        path.as_ref(),
                        K,
                        offset,
                        descending,
                        None,
                        grove_version,
                    )
                    .unwrap()
                    .expect("paginated page");
                let start = (offset as usize).min(N as usize);
                let end = (start + K as usize).min(N as usize);
                assert_eq!(
                    page.entries,
                    full[start..end],
                    "page mismatch at offset={offset} descending={descending}"
                );
                assert_eq!(
                    page.skipped,
                    offset.min(N),
                    "true skipped at offset={offset}"
                );
            }
        }
    }

    // -----------------------------------------------------------------
    // Cross-path parity: the unproved `skipped` is the same quantity the
    // proved path attests.
    //
    // A client reading `skipped` off the wire cannot tell which path
    // served it, so the two must report the same number for the same
    // request — including the past-the-end shape, where both must report
    // the population rather than echoing the request. The proved side
    // re-derives its value from the counted subtree commitments in the
    // proof bytes; the unproved side reads the secondary's root
    // aggregate. Nothing structural keeps those in step, so it is pinned
    // here rather than assumed.
    // -----------------------------------------------------------------

    #[test]
    fn unproved_skipped_equals_the_proved_paths_attested_skipped() {
        let grove_version = GroveVersion::latest();
        let (db, population) = make_pcit_with_mixed_counts(grove_version);
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let pop = population as u64;

        for descending in [false, true] {
            // Offsets spanning: nothing skipped, mid-page, the last row,
            // exactly the population, just past it, and absurdly past it
            // (the original denial-of-service lever).
            for offset in [0u64, 1, 5, pop - 1, pop, pop + 1, 4_000_000_000] {
                for k in [0u16, 1, 3] {
                    let unproved = db
                        .indexed_count_top_k_paginated(
                            path,
                            k,
                            offset,
                            descending,
                            None,
                            grove_version,
                        )
                        .unwrap()
                        .expect("unproved page");

                    let proof = db
                        .prove_indexed_count_top_k_paginated(
                            path,
                            k,
                            offset,
                            descending,
                            None,
                            grove_version,
                        )
                        .unwrap()
                        .expect("prove page");
                    let proved = GroveDb::verify_indexed_count_top_k_paginated(
                        &proof, path, k, offset, descending,
                    )
                    .expect("verify page");

                    assert_eq!(
                        unproved.skipped, proved.skipped,
                        "skipped disagrees between paths at offset={offset} k={k} \
                         descending={descending}"
                    );
                    // Both paths must also agree that `min(offset,
                    // population)` is what that number means — equality
                    // alone would be satisfied by two identically wrong
                    // values.
                    assert_eq!(
                        unproved.skipped,
                        offset.min(pop),
                        "skipped is not min(offset, population) at offset={offset} k={k} \
                         descending={descending}"
                    );

                    let proved_entries = match &proved.entries {
                        AxisEntries::Count(v) => v.clone(),
                        other => panic!("count axis proof returned {other:?}"),
                    };
                    assert_eq!(
                        unproved.entries, proved_entries,
                        "entries disagree between paths at offset={offset} k={k} \
                         descending={descending}"
                    );
                }
            }
        }
    }

    /// Always-on differential against the pre-change implementation
    /// (`legacy_linear_indexed_count_top_k_paginated`, kept verbatim as a
    /// test-only baseline): identical entries in identical order at every
    /// probed offset/k/direction, plus the `skipped` semantics the legacy
    /// shape could not report. The release-mode measurement harness runs
    /// the same comparison at 1e3–1e6 rows; this pins it in CI at a size
    /// CI can afford.
    #[test]
    fn counted_path_agrees_with_the_pre_change_linear_implementation() {
        let grove_version = GroveVersion::latest();
        const ROWS: u64 = 120;
        let db = make_pcit_with_rows(ROWS, grove_version);
        let path = [TEST_LEAF, b"cidx"];

        for descending in [false, true] {
            for offset in [0u64, 1, 3, 60, ROWS - 1, ROWS, ROWS + 80] {
                for k in [1u16, 7] {
                    let counted = db
                        .indexed_count_top_k_paginated(
                            path.as_ref(),
                            k,
                            offset,
                            descending,
                            None,
                            grove_version,
                        )
                        .unwrap()
                        .expect("counted read");
                    let legacy = db
                        .legacy_linear_indexed_count_top_k_paginated(
                            path.as_ref(),
                            k,
                            offset,
                            descending,
                            None,
                            grove_version,
                        )
                        .unwrap()
                        .expect("legacy linear read");
                    assert_eq!(
                        counted.entries, legacy,
                        "counted and legacy diverge at offset={offset} k={k} \
                         descending={descending}"
                    );
                    assert_eq!(
                        counted.skipped,
                        offset.min(ROWS),
                        "true skipped at offset={offset} k={k} descending={descending}"
                    );
                }
            }
        }
    }

    // -----------------------------------------------------------------
    // Manual measurement harness — not run in CI. Run with:
    //   cargo test -p grovedb measure_paginated_costs -- --ignored --nocapture
    // -----------------------------------------------------------------

    /// Measures the counted paginated read against the pre-change linear
    /// loop (kept verbatim as
    /// `GroveDb::legacy_linear_indexed_count_top_k_paginated`) on the
    /// N-group Count-axis secondary shape, and prints `OperationCost`
    /// counters plus wall-clock. The counters are the trustworthy,
    /// machine-independent signal; wall-clock is reported because it is
    /// what a node operator feels, but it is noisy on a loaded machine —
    /// check the load average before quoting it.
    #[test]
    #[ignore]
    fn measure_paginated_costs() {
        use std::time::Instant;

        let grove_version = GroveVersion::latest();
        println!();
        println!(
            "wall-clock is min of 3 runs on a shared machine — treat seeks/bytes as the signal"
        );
        println!("| n | k | offset | path | seeks | loaded bytes | rows | wall µs |");
        println!("|---|---|---|---|---|---|---|---|");

        for n in [1_000u64, 10_000, 100_000, 1_000_000] {
            let db = make_pcit_with_rows(n, grove_version);
            let path = [TEST_LEAF, b"cidx"];
            for k in [1u16, 100] {
                // `offset = 1` is the worst case for the counted collect
                // phase: the whole page is gathered through tree-node
                // point-gets instead of sequential iterator steps.
                for offset in [0u64, 1, n - 1, n, 4_000_000_000] {
                    let mut counted_wall = u128::MAX;
                    let mut counted = None;
                    for _ in 0..3 {
                        let started = Instant::now();
                        let run = db.indexed_count_top_k_paginated(
                            path.as_ref(),
                            k,
                            offset,
                            false,
                            None,
                            grove_version,
                        );
                        counted_wall = counted_wall.min(started.elapsed().as_micros());
                        counted = Some(run);
                    }
                    let CostContext {
                        value: counted_rows,
                        cost: counted_cost,
                    } = counted.expect("three runs happened");
                    let counted_page = counted_rows.expect("counted read");

                    let mut linear_wall = u128::MAX;
                    let mut linear = None;
                    for _ in 0..3 {
                        let started = Instant::now();
                        let run = db.legacy_linear_indexed_count_top_k_paginated(
                            path.as_ref(),
                            k,
                            offset,
                            false,
                            None,
                            grove_version,
                        );
                        linear_wall = linear_wall.min(started.elapsed().as_micros());
                        linear = Some(run);
                    }
                    let CostContext {
                        value: linear_rows,
                        cost: linear_cost,
                    } = linear.expect("three runs happened");
                    let linear_rows = linear_rows.expect("legacy linear read");

                    assert_eq!(
                        counted_page.entries, linear_rows,
                        "counted and linear paths diverged at n={n} k={k} offset={offset}"
                    );
                    assert_eq!(
                        counted_page.skipped,
                        offset.min(n),
                        "true skipped at n={n} k={k} offset={offset}"
                    );

                    println!(
                        "| {n} | {k} | {offset} | counted | {} | {} | {} | {} |",
                        counted_cost.seek_count,
                        counted_cost.storage_loaded_bytes,
                        counted_page.entries.len(),
                        counted_wall,
                    );
                    println!(
                        "| {n} | {k} | {offset} | linear | {} | {} | {} | {} |",
                        linear_cost.seek_count,
                        linear_cost.storage_loaded_bytes,
                        linear_rows.len(),
                        linear_wall,
                    );
                }
            }
        }
    }
}
