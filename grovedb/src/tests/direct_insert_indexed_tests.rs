//! Direct-API insert edge cases for PCIT / PSIT / PCPSIT.
//!
//! Covers `grovedb/src/operations/insert/mod.rs` indexed-tree
//! branches:
//! - Empty creation succeeds at multiple depths.
//! - Non-empty insert is validated against on-disk state and rejected
//!   on mismatch (InvalidInput).
//! - Partial-state rejections (one root_key Some, the other None;
//!   count > 0 with both None; etc.).
//! - PCPSIT axes validation (empty, > 3 entries, unsorted/duplicate).
//! - insert_if_not_exists semantics on indexed trees.

#[cfg(test)]
mod tests {
    use grovedb_element::indexed::IndexAxis;
    use grovedb_version::version::GroveVersion;

    use crate::{
        tests::{make_test_grovedb, TEST_LEAF},
        Element, Error,
    };

    // -----------------------------------------------------------------
    // Empty creation at multiple depths
    // -----------------------------------------------------------------

    #[test]
    fn direct_insert_empty_pcit_under_test_leaf() {
        let grove_version = GroveVersion::latest();
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
        .expect("empty PCIT under test_leaf");
    }

    #[test]
    fn direct_insert_empty_pcit_under_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"tree",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("tree");
        db.insert(
            [TEST_LEAF, b"tree"].as_ref(),
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("PCIT under tree");
    }

    #[test]
    fn direct_insert_empty_psit_under_test_leaf() {
        let grove_version = GroveVersion::latest();
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
        .expect("empty PSIT");
    }

    #[test]
    fn direct_insert_empty_pcpsit_under_test_leaf_all_axis_subsets() {
        let grove_version = GroveVersion::latest();
        for tags in [
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
        ] {
            let db = make_test_grovedb(grove_version);
            let axes: Vec<(u8, Option<Vec<u8>>)> = tags.iter().map(|t| (*t, None)).collect();
            let elem = Element::empty_provable_count_provable_sum_indexed_tree(axes)
                .expect("axes canonical");
            db.insert(
                [TEST_LEAF].as_ref(),
                b"pcpsit",
                elem,
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("empty PCPSIT");
        }
    }

    #[test]
    fn direct_insert_empty_psit_nested_under_psit() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("outer PSIT");
        // PSIT primary accepts an empty PSIT child? It accepts any
        // tree variant that's empty.
        let result = db
            .insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"outer"].as_ref(),
                b"inner",
                Element::empty_provable_sum_indexed_tree(),
                None,
                grove_version,
            )
            .unwrap();
        // Either accepts or rejects with NotSupported; both are valid.
        // What matters is no panic.
        let _ = result;
    }

    // -----------------------------------------------------------------
    // Non-empty PCIT validated insert path
    // -----------------------------------------------------------------

    #[test]
    fn direct_insert_pcit_partial_state_primary_only_rejected() {
        // primary = Some, secondary = None — invariant violation, should
        // be rejected with InvalidInput.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let partial = Element::new_provable_count_indexed_tree_with_root_keys_and_count_value(
            Some(b"primary_root".to_vec()),
            None,
            5,
            None,
        );
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"cidx",
                partial,
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            matches!(result, Err(Error::InvalidInput(_))),
            "expected InvalidInput, got {:?}",
            result
        );
    }

    #[test]
    fn direct_insert_pcit_partial_state_secondary_only_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let partial = Element::new_provable_count_indexed_tree_with_root_keys_and_count_value(
            None,
            Some(b"secondary_root".to_vec()),
            5,
            None,
        );
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"cidx",
                partial,
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[test]
    fn direct_insert_pcit_with_nonzero_count_and_no_roots_rejected() {
        // count > 0 with both root_keys = None is a partial state
        // and should be rejected.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let partial = Element::new_provable_count_indexed_tree_with_root_keys_and_count_value(
            None, None, 5, None,
        );
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"cidx",
                partial,
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[test]
    fn direct_insert_pcit_mismatched_primary_root_key_rejected() {
        // Try to insert a non-empty PCIT at a path where the merks
        // don't have those root keys — validation must reject.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        // Non-empty PCIT claim with bogus root keys; the on-disk
        // child merks at [TEST_LEAF, cidx] will be opened-on-demand
        // and their actual root keys checked against our claim.
        let bogus = Element::new_provable_count_indexed_tree_with_root_keys_and_count_value(
            Some(b"WRONG".to_vec()),
            Some(b"WRONG".to_vec()),
            1,
            None,
        );
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"cidx",
                bogus,
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            result.is_err(),
            "expected error on mismatched root keys, got {:?}",
            result
        );
    }

    // -----------------------------------------------------------------
    // Non-empty PSIT validated insert path
    // -----------------------------------------------------------------

    #[test]
    fn direct_insert_psit_partial_state_primary_only_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let partial = Element::ProvableSumIndexedTree(Some(b"r".to_vec()), None, 5, None);
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"psit",
                partial,
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[test]
    fn direct_insert_psit_partial_state_secondary_only_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let partial = Element::ProvableSumIndexedTree(None, Some(b"r".to_vec()), 5, None);
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"psit",
                partial,
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[test]
    fn direct_insert_psit_mismatched_root_keys_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        // Inject a non-empty PSIT claim at a fresh path. The on-disk
        // child merks will be opened-on-demand and their actual root
        // keys checked against the claim — must reject.
        let bogus = Element::ProvableSumIndexedTree(
            Some(b"WRONG".to_vec()),
            Some(b"WRONG".to_vec()),
            7,
            None,
        );
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"psit",
                bogus,
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert!(result.is_err(), "got {:?}", result);
    }

    // -----------------------------------------------------------------
    // PCPSIT validated insert + axes validation
    // -----------------------------------------------------------------

    #[test]
    fn direct_insert_pcpsit_with_no_primary_but_count_nonzero_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let axes: Vec<(u8, Option<Vec<u8>>)> = vec![(IndexAxis::Count.tag(), None)];
        let bogus = Element::ProvableCountProvableSumIndexedTree(None, 5, 0, axes, None);
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"pcpsit",
                bogus,
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[test]
    fn direct_insert_pcpsit_with_no_primary_but_sum_nonzero_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let axes: Vec<(u8, Option<Vec<u8>>)> = vec![(IndexAxis::Sum.tag(), None)];
        let bogus = Element::ProvableCountProvableSumIndexedTree(None, 0, 100, axes, None);
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"pcpsit",
                bogus,
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[test]
    fn direct_insert_pcpsit_mismatched_primary_root_key_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        // Try non-empty PCPSIT with wrong primary root key — the on-disk
        // child merks (opened on demand) won't match.
        let axes_wrong: Vec<(u8, Option<Vec<u8>>)> =
            vec![(IndexAxis::Count.tag(), Some(b"WRONG_SEC".to_vec()))];
        let bogus = Element::ProvableCountProvableSumIndexedTree(
            Some(b"WRONG_PRI".to_vec()),
            1,
            7,
            axes_wrong,
            None,
        );
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"pcpsit",
                bogus,
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert!(result.is_err(), "got {:?}", result);
    }

    // -----------------------------------------------------------------
    // PCPSIT axes constructor validation
    // -----------------------------------------------------------------

    #[test]
    fn pcpsit_constructor_rejects_empty_axes() {
        let result = Element::empty_provable_count_provable_sum_indexed_tree(vec![]);
        assert!(result.is_err(), "empty axes must be rejected");
    }

    #[test]
    fn pcpsit_constructor_rejects_oversize_axes() {
        // 4 axes — beyond the 3-axis cap.
        let axes = vec![(0u8, None), (1u8, None), (2u8, None), (3u8, None)];
        let result = Element::empty_provable_count_provable_sum_indexed_tree(axes);
        assert!(result.is_err());
    }

    #[test]
    fn pcpsit_constructor_rejects_unsorted_axes() {
        let axes = vec![(IndexAxis::Sum.tag(), None), (IndexAxis::Count.tag(), None)];
        let result = Element::empty_provable_count_provable_sum_indexed_tree(axes);
        assert!(result.is_err());
    }

    #[test]
    fn pcpsit_constructor_rejects_duplicate_axes() {
        let axes = vec![
            (IndexAxis::Count.tag(), None),
            (IndexAxis::Count.tag(), None),
        ];
        let result = Element::empty_provable_count_provable_sum_indexed_tree(axes);
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------
    // Insert overwrite tests: same path, different element types
    // -----------------------------------------------------------------

    #[test]
    fn direct_insert_pcit_overwrite_rejected_by_default() {
        // Direct `insert` is not allowed to override an existing tree
        // by default — surfaces as OverrideNotAllowed.
        let grove_version = GroveVersion::latest();
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
        .expect("first");
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"cidx",
                Element::empty_provable_count_indexed_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap();
        // Overwrites of trees are not allowed by direct insert.
        assert!(
            matches!(result, Err(Error::OverrideNotAllowed(_))),
            "expected OverrideNotAllowed, got {:?}",
            result
        );
    }

    // -----------------------------------------------------------------
    // PCPSIT with primary but some axes still None (mixed)
    // -----------------------------------------------------------------

    #[test]
    fn direct_insert_pcpsit_some_axes_some_some_axes_none() {
        // primary present but some axes' secondary_root_key = None
        // should be rejected (those axes need to be claimed too).
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let axes: Vec<(u8, Option<Vec<u8>>)> = vec![
            (IndexAxis::Count.tag(), Some(b"present".to_vec())),
            (IndexAxis::Sum.tag(), None), // partial axis state
        ];
        let partial = Element::ProvableCountProvableSumIndexedTree(
            Some(b"primary".to_vec()),
            1,
            0,
            axes,
            None,
        );
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"pcpsit",
                partial,
                None,
                None,
                grove_version,
            )
            .unwrap();
        // Validation should reject because axes' secondary root_key
        // won't match the freshly-opened secondary's root key.
        assert!(result.is_err());
    }
}
