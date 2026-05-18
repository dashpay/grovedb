//! `ProvableCountProvableSumIndexedTree` (PCPSIT) tests.
//!
//! Phase 2 coverage for the multi-axis indexed tree:
//! - Empty creation + `verify_grovedb` passes (single-axis and
//!   multi-axis variants).
//! - Single insert + read-back.
//! - Multiple inserts + `verify_grovedb` passes.
//! - Delete + `verify_grovedb` still passes.
//! - Child-type rejection: PCPSIT must reject items that contribute
//!   only count OR only sum (e.g. `Item`, `SumItem`).
//! - For each axis combination (count, sum, avg, count+sum, count+avg,
//!   sum+avg, all-three): verify_grovedb passes after inserts.
//! - Avg axis 0/0 invariant: an empty PCPSIT with avg axis configured
//!   must verify.

#[cfg(test)]
mod tests {
    use grovedb_element::indexed::IndexAxis;
    use grovedb_version::version::GroveVersion;

    use crate::{
        tests::{make_test_grovedb, TEST_LEAF},
        Element,
    };

    fn insert_empty_pcpsit(
        db: &crate::GroveDb,
        key: &[u8],
        axis_tags: &[u8],
        grove_version: &GroveVersion,
    ) {
        let axes: Vec<(u8, Option<Vec<u8>>)> = axis_tags.iter().map(|t| (*t, None)).collect();
        let elem = Element::empty_provable_count_provable_sum_indexed_tree(axes)
            .expect("axes are canonical");
        db.insert([TEST_LEAF].as_ref(), key, elem, None, None, grove_version)
            .unwrap()
            .expect("PCPSIT insertion should succeed");
    }

    #[test]
    fn pcpsit_empty_creation_verify_passes_all_axis_combinations() {
        let grove_version = GroveVersion::latest();
        // Each non-empty subset of {count, sum, avg} is a valid axes
        // selection. 2^3 - 1 = 7 combinations.
        let combinations: [&[u8]; 7] = [
            &[IndexAxis::Count.tag()],
            &[IndexAxis::Sum.tag()],
            &[IndexAxis::Avg.tag()],
            &[IndexAxis::Count.tag(), IndexAxis::Sum.tag()],
            &[IndexAxis::Count.tag(), IndexAxis::Avg.tag()],
            &[IndexAxis::Sum.tag(), IndexAxis::Avg.tag()],
            &[
                IndexAxis::Count.tag(),
                IndexAxis::Sum.tag(),
                IndexAxis::Avg.tag(),
            ],
        ];
        for (i, tags) in combinations.iter().enumerate() {
            let db = make_test_grovedb(grove_version);
            let key = format!("pcpsit_{}", i);
            insert_empty_pcpsit(&db, key.as_bytes(), tags, grove_version);
            let issues = db
                .verify_grovedb(None, true, true, grove_version)
                .expect("verify_grovedb should succeed");
            assert!(
                issues.is_empty(),
                "verify_grovedb reported issues for empty PCPSIT with axes {:?}: {:?}",
                tags,
                issues
            );
        }
    }

    #[test]
    fn pcpsit_single_insert_then_read_back() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit(
            &db,
            b"pcpsit",
            &[
                IndexAxis::Count.tag(),
                IndexAxis::Sum.tag(),
                IndexAxis::Avg.tag(),
            ],
            grove_version,
        );

        // ItemWithSumItem contributes (count=1, sum=value).
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"pcpsit"].as_ref(),
            b"row1",
            Element::new_item_with_sum_item(b"hello".to_vec(), 42),
            None,
            grove_version,
        )
        .unwrap()
        .expect("PCPSIT insertion of ItemWithSumItem should succeed");

        let read = db
            .get(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                b"row1",
                None,
                grove_version,
            )
            .unwrap()
            .expect("get should return the inserted item");
        assert_eq!(read, Element::new_item_with_sum_item(b"hello".to_vec(), 42));

        let issues = db
            .verify_grovedb(None, true, true, grove_version)
            .expect("verify_grovedb should succeed");
        assert!(
            issues.is_empty(),
            "verify_grovedb reported issues after single PCPSIT insert: {:?}",
            issues
        );
    }

    #[test]
    fn pcpsit_multiple_inserts_verify_passes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit(
            &db,
            b"pcpsit",
            &[
                IndexAxis::Count.tag(),
                IndexAxis::Sum.tag(),
                IndexAxis::Avg.tag(),
            ],
            grove_version,
        );

        for (key, sum) in [
            (b"a".as_ref(), 10i64),
            (b"b".as_ref(), -5),
            (b"c".as_ref(), 100),
            (b"d".as_ref(), 0),
        ] {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                key,
                Element::new_item_with_sum_item(vec![1, 2, 3], sum),
                None,
                grove_version,
            )
            .unwrap()
            .expect("PCPSIT insertion should succeed");
        }

        let issues = db
            .verify_grovedb(None, true, true, grove_version)
            .expect("verify_grovedb should succeed");
        assert!(
            issues.is_empty(),
            "verify_grovedb reported issues after multiple PCPSIT inserts: {:?}",
            issues
        );
    }

    #[test]
    fn pcpsit_delete_then_verify_passes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit(
            &db,
            b"pcpsit",
            &[IndexAxis::Count.tag(), IndexAxis::Sum.tag()],
            grove_version,
        );

        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"pcpsit"].as_ref(),
            b"row1",
            Element::new_item_with_sum_item(b"a".to_vec(), 42),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"pcpsit"].as_ref(),
            b"row2",
            Element::new_item_with_sum_item(b"b".to_vec(), -7),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");

        let removed = db
            .delete_from_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                b"row1",
                None,
                grove_version,
            )
            .unwrap()
            .expect("delete");
        assert!(removed);

        let removed_again = db
            .delete_from_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                b"row1",
                None,
                grove_version,
            )
            .unwrap()
            .expect("delete of missing key returns false");
        assert!(!removed_again);

        let issues = db
            .verify_grovedb(None, true, true, grove_version)
            .expect("verify_grovedb should succeed");
        assert!(
            issues.is_empty(),
            "verify_grovedb reported issues after PCPSIT delete: {:?}",
            issues
        );
    }

    #[test]
    fn pcpsit_rejects_count_only_item() {
        // A plain Item contributes only count = 1 (no sum). PCPSIT
        // requires children to contribute BOTH count and sum, so a
        // plain Item must be rejected.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit(
            &db,
            b"pcpsit",
            &[IndexAxis::Count.tag(), IndexAxis::Sum.tag()],
            grove_version,
        );

        let result = db
            .insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                b"row1",
                Element::new_item(b"plain".to_vec()),
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            result.is_err(),
            "PCPSIT must reject a plain Item child, got: {:?}",
            result
        );
    }

    #[test]
    fn pcpsit_rejects_sum_only_item() {
        // A plain SumItem contributes (count=1, sum=value) — but
        // `is_count_and_sum_bearing_child` requires elements whose
        // ergonomic role is "both axes" (ItemWithSumItem,
        // ReferenceWithSumItem, count+sum trees). The simpler SumItem
        // path is rejected; callers wanting sum-only writes should
        // use the PSIT variant instead.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit(
            &db,
            b"pcpsit",
            &[IndexAxis::Count.tag(), IndexAxis::Sum.tag()],
            grove_version,
        );

        let result = db
            .insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                b"row1",
                Element::new_sum_item(42),
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            result.is_err(),
            "PCPSIT must reject a SumItem-only child, got: {:?}",
            result
        );
    }

    /// Pin the avg axis's 0/0 invariant: an empty PCPSIT with the avg
    /// axis configured (count = 0, sum = 0) must produce a stable hash
    /// chain that verifies. This is the special case where
    /// `compute_avg_fixed_point(0, 0) = 0` by convention.
    #[test]
    fn pcpsit_avg_axis_0_over_0_empty_verify_passes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        insert_empty_pcpsit(&db, b"pcpsit", &[IndexAxis::Avg.tag()], grove_version);
        let issues = db
            .verify_grovedb(None, true, true, grove_version)
            .expect("verify_grovedb");
        assert!(
            issues.is_empty(),
            "verify_grovedb reported issues for empty PCPSIT(avg): {:?}",
            issues
        );
    }
}
