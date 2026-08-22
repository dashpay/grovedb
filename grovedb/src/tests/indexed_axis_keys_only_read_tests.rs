//! Keys-only indexed-axis reads: `indexed_<axis>_{top_k,top_k_paginated,
//! range}_keys` return the ranking pairs straight from the secondary view
//! and never open the primary. They must agree with the resolving reads
//! on the page (same `_rows_generic` core), and cost strictly less.

#[cfg(test)]
mod tests {
    use grovedb_element::indexed::IndexAxis;
    use grovedb_version::version::GroveVersion;

    use crate::{
        query_result_type::IndexedAxisEntrySliceExt,
        tests::{make_test_grovedb, TEST_LEAF},
        Element, GroveDb,
    };

    const PCPSIT: &[u8] = b"pcpsit";

    /// A three-axis (count, sum, avg) indexed tree with `(key, sum)`
    /// entries; count is 1 per entry, so the count axis orders by key.
    fn build(db: &GroveDb, gv: &GroveVersion, entries: &[(&[u8], i64)]) {
        let axes: Vec<(u8, Option<Vec<u8>>)> = vec![
            (IndexAxis::Count.tag(), None),
            (IndexAxis::Sum.tag(), None),
            (IndexAxis::Avg.tag(), None),
        ];
        db.insert(
            [TEST_LEAF].as_ref(),
            PCPSIT,
            Element::empty_provable_count_provable_sum_indexed_tree(axes).expect("canonical axes"),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("create pcpsit");
        for (key, sum) in entries {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, PCPSIT].as_ref(),
                key,
                Element::new_item_with_sum_item(b"v".to_vec(), *sum),
                None,
                gv,
            )
            .unwrap()
            .expect("insert entry");
        }
    }

    fn path() -> Vec<&'static [u8]> {
        vec![TEST_LEAF, PCPSIT]
    }

    /// The paginated keys-only page equals the resolving page projected
    /// to its ranking pairs, including the skipped count, on every axis,
    /// both directions, with and without an offset.
    #[test]
    fn paginated_keys_agree_with_resolving_pages_on_every_axis() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build(
            &db,
            gv,
            &[(b"a", 50), (b"b", 10), (b"c", 30), (b"d", 20), (b"e", 40)],
        );

        for descending in [true, false] {
            for offset in [0u64, 2] {
                let p = path();
                let (sum_keys, sum_full) = (
                    db.indexed_sum_top_k_paginated_keys(
                        p.as_slice(),
                        2,
                        offset,
                        descending,
                        None,
                        gv,
                    )
                    .unwrap()
                    .expect("sum keys"),
                    db.indexed_sum_top_k_paginated(p.as_slice(), 2, offset, descending, None, gv)
                        .unwrap()
                        .expect("sum full"),
                );
                assert_eq!(sum_keys.entries, sum_full.entries.key_pairs());
                assert_eq!(sum_keys.skipped, sum_full.skipped);

                let (count_keys, count_full) = (
                    db.indexed_count_top_k_paginated_keys(
                        p.as_slice(),
                        2,
                        offset,
                        descending,
                        None,
                        gv,
                    )
                    .unwrap()
                    .expect("count keys"),
                    db.indexed_count_top_k_paginated(p.as_slice(), 2, offset, descending, None, gv)
                        .unwrap()
                        .expect("count full"),
                );
                assert_eq!(count_keys.entries, count_full.entries.key_pairs());
                assert_eq!(count_keys.skipped, count_full.skipped);

                let (avg_keys, avg_full) = (
                    db.indexed_avg_top_k_paginated_keys(
                        p.as_slice(),
                        2,
                        offset,
                        descending,
                        None,
                        gv,
                    )
                    .unwrap()
                    .expect("avg keys"),
                    db.indexed_avg_top_k_paginated(p.as_slice(), 2, offset, descending, None, gv)
                        .unwrap()
                        .expect("avg full"),
                );
                assert_eq!(avg_keys.entries, avg_full.entries.key_pairs());
                assert_eq!(avg_keys.skipped, avg_full.skipped);
            }
        }
    }

    /// Range and plain top-k keys-only reads agree with their resolving
    /// counterparts.
    #[test]
    fn range_and_top_k_keys_agree_with_resolving_reads() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build(
            &db,
            gv,
            &[(b"a", 50), (b"b", 10), (b"c", 30), (b"d", 20), (b"e", 40)],
        );
        let p = path();

        let keys = db
            .indexed_sum_range_keys(p.as_slice(), 15, 45, false, 10, None, gv)
            .unwrap()
            .expect("sum range keys");
        let full = db
            .indexed_sum_range(p.as_slice(), 15, 45, false, 10, None, gv)
            .unwrap()
            .expect("sum range full");
        assert_eq!(keys, full.key_pairs());
        assert_eq!(
            keys,
            vec![
                (20, b"d".to_vec()),
                (30, b"c".to_vec()),
                (40, b"e".to_vec())
            ]
        );

        let keys = db
            .indexed_count_range_keys(p.as_slice(), 1, 1, true, 10, None, gv)
            .unwrap()
            .expect("count range keys");
        let full = db
            .indexed_count_range(p.as_slice(), 1, 1, true, 10, None, gv)
            .unwrap()
            .expect("count range full");
        assert_eq!(keys, full.key_pairs());

        let keys = db
            .indexed_avg_range_keys(p.as_slice(), i128::MIN, i128::MAX, true, 3, None, gv)
            .unwrap()
            .expect("avg range keys");
        let full = db
            .indexed_avg_range(p.as_slice(), i128::MIN, i128::MAX, true, 3, None, gv)
            .unwrap()
            .expect("avg range full");
        assert_eq!(keys, full.key_pairs());

        let keys = db
            .indexed_sum_top_k_keys(p.as_slice(), 3, true, None, gv)
            .unwrap()
            .expect("sum top k keys");
        let full = db
            .indexed_sum_top_k(p.as_slice(), 3, true, None, gv)
            .unwrap()
            .expect("sum top k full");
        assert_eq!(keys, full.key_pairs());
        let keys = db
            .indexed_count_top_k_keys(p.as_slice(), 3, false, None, gv)
            .unwrap()
            .expect("count top k keys");
        let full = db
            .indexed_count_top_k(p.as_slice(), 3, false, None, gv)
            .unwrap()
            .expect("count top k full");
        assert_eq!(keys, full.key_pairs());
        let keys = db
            .indexed_avg_top_k_keys(p.as_slice(), 3, true, None, gv)
            .unwrap()
            .expect("avg top k keys");
        let full = db
            .indexed_avg_top_k(p.as_slice(), 3, true, None, gv)
            .unwrap()
            .expect("avg top k full");
        assert_eq!(keys, full.key_pairs());
    }

    /// A keys-only read never opens the primary, so it costs strictly
    /// fewer seeks than the resolving read of the same page (which pays
    /// one primary read per entry), and degenerate bounds stay the same
    /// empty answer.
    #[test]
    fn keys_only_reads_skip_the_primary_reads() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build(
            &db,
            gv,
            &[(b"a", 50), (b"b", 10), (b"c", 30), (b"d", 20), (b"e", 40)],
        );
        let p = path();

        let keys_cost = db
            .indexed_sum_top_k_paginated_keys(p.as_slice(), 3, 1, true, None, gv)
            .cost;
        let full_cost = db
            .indexed_sum_top_k_paginated(p.as_slice(), 3, 1, true, None, gv)
            .cost;
        assert!(
            keys_cost.seek_count < full_cost.seek_count,
            "keys-only must not pay the primary reads: {} vs {}",
            keys_cost.seek_count,
            full_cost.seek_count
        );
        assert!(keys_cost.storage_loaded_bytes < full_cost.storage_loaded_bytes);

        let keys_cost = db
            .indexed_sum_range_keys(p.as_slice(), 0, 100, false, 10, None, gv)
            .cost;
        let full_cost = db
            .indexed_sum_range(p.as_slice(), 0, 100, false, 10, None, gv)
            .cost;
        assert!(keys_cost.seek_count < full_cost.seek_count);

        let inverted = db
            .indexed_sum_range_keys(p.as_slice(), 10, 5, false, 10, None, gv)
            .unwrap()
            .expect("inverted bounds");
        assert!(inverted.is_empty());
    }
}
