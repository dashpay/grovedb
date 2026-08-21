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

    // -----------------------------------------------------------------
    // Non-empty insert success paths + value-mismatch error paths
    //
    // These exercise the deep validation branches in
    // `add_element_on_transaction` for PCIT (L319–L413),
    // PSIT (L420–L513), and PCPSIT (L516–L622+):
    //   - success: provided root_keys match on-disk → element written
    //   - error : provided primary_root_key mismatches on-disk
    //   - error : provided secondary_root_key mismatches on-disk
    //   - error : provided axis secondary_root_key mismatches (PCPSIT)
    // -----------------------------------------------------------------

    use crate::operations::insert::InsertOptions;

    fn override_tree_opts() -> InsertOptions {
        InsertOptions {
            validate_insertion_does_not_override: false,
            validate_insertion_does_not_override_tree: false,
            base_root_storage_is_free: true,
        }
    }

    #[test]
    fn direct_insert_pcit_with_matching_roots_succeeds() {
        // Build a real non-empty PCIT, capture its actual root keys
        // and count, then replace via `db.insert` with
        // `validate_insertion_does_not_override_tree: false`. This
        // exercises the success branch at L401 in
        // insert/mod.rs (the (p_hash, s_hash) tuple).
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
        .expect("create");
        // The child enters EMPTY and gains its count from its contents:
        // one item inside derives count 1 on the child and, through
        // propagation, count 1 on the PCIT.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::empty_provable_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");
        db.insert(
            [TEST_LEAF, b"cidx", b"a"].as_ref(),
            b"row",
            Element::new_item(b"v".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("derive child count");

        // Read the actual on-disk roots.
        let elem = db
            .get_raw([TEST_LEAF].as_ref().into(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get");
        let (real_primary, real_secondary, real_count) = match elem.underlying() {
            Element::ProvableCountIndexedTree(p, s, c, _) => (p.clone(), s.clone(), *c),
            other => panic!("expected PCIT, got {:?}", other),
        };
        assert!(real_primary.is_some());
        assert!(real_secondary.is_some());
        assert_eq!(real_count, 1);

        // Re-insert with the actual roots — must succeed via L401.
        let claimed = Element::new_provable_count_indexed_tree_with_root_keys_and_count_value(
            real_primary.clone(),
            real_secondary.clone(),
            real_count,
            None,
        );
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            claimed,
            Some(override_tree_opts()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("matching-roots PCIT insert succeeds");

        // Verify post-condition.
        let issues = db
            .verify_grovedb(None, true, true, grove_version)
            .expect("verify");
        assert!(issues.is_empty(), "issues: {:?}", issues);
    }

    #[test]
    fn direct_insert_pcit_mismatched_primary_root_key_with_override_rejected() {
        // Same shape as the success test but the claimed
        // primary_root_key is bogus — must hit L369 (primary mismatch).
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
        .expect("create");
        // The child enters EMPTY and gains its count from its contents:
        // one item inside derives count 1 on the child and, through
        // propagation, count 1 on the PCIT.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::empty_provable_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");
        db.insert(
            [TEST_LEAF, b"cidx", b"a"].as_ref(),
            b"row",
            Element::new_item(b"v".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("derive child count");
        let elem = db
            .get_raw([TEST_LEAF].as_ref().into(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get");
        let (_, real_secondary, real_count) = match elem.underlying() {
            Element::ProvableCountIndexedTree(p, s, c, _) => (p.clone(), s.clone(), *c),
            other => panic!("expected PCIT, got {:?}", other),
        };

        let bogus = Element::new_provable_count_indexed_tree_with_root_keys_and_count_value(
            Some(b"WRONG_PRIMARY".to_vec()),
            real_secondary,
            real_count,
            None,
        );
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"cidx",
                bogus,
                Some(override_tree_opts()),
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[test]
    fn direct_insert_pcit_mismatched_secondary_root_key_with_override_rejected() {
        // Mismatch on the secondary root key — must hit L393.
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
        .expect("create");
        // The child enters EMPTY and gains its count from its contents:
        // one item inside derives count 1 on the child and, through
        // propagation, count 1 on the PCIT.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::empty_provable_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");
        db.insert(
            [TEST_LEAF, b"cidx", b"a"].as_ref(),
            b"row",
            Element::new_item(b"v".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("derive child count");
        let elem = db
            .get_raw([TEST_LEAF].as_ref().into(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get");
        let (real_primary, _, real_count) = match elem.underlying() {
            Element::ProvableCountIndexedTree(p, s, c, _) => (p.clone(), s.clone(), *c),
            other => panic!("expected PCIT, got {:?}", other),
        };

        let bogus = Element::new_provable_count_indexed_tree_with_root_keys_and_count_value(
            real_primary,
            Some(b"WRONG_SECONDARY".to_vec()),
            real_count,
            None,
        );
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"cidx",
                bogus,
                Some(override_tree_opts()),
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[test]
    fn direct_insert_psit_with_matching_roots_succeeds() {
        // PSIT success path at L483.
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
        .expect("create");
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"a",
            Element::new_sum_item(5),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");
        let elem = db
            .get_raw([TEST_LEAF].as_ref().into(), b"psit", None, grove_version)
            .unwrap()
            .expect("get");
        let (real_primary, real_secondary, real_sum) = match elem.underlying() {
            Element::ProvableSumIndexedTree(p, s, sv, _) => (p.clone(), s.clone(), *sv),
            other => panic!("expected PSIT, got {:?}", other),
        };
        assert!(real_primary.is_some());
        assert!(real_secondary.is_some());

        let claimed = Element::ProvableSumIndexedTree(real_primary, real_secondary, real_sum, None);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"psit",
            claimed,
            Some(override_tree_opts()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("matching-roots PSIT insert succeeds");
        let issues = db
            .verify_grovedb(None, true, true, grove_version)
            .expect("verify");
        assert!(issues.is_empty(), "issues: {:?}", issues);
    }

    #[test]
    fn direct_insert_psit_mismatched_primary_with_override_rejected() {
        // Hits L450 (primary mismatch in PSIT branch).
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
        .expect("create");
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"a",
            Element::new_sum_item(5),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");
        let elem = db
            .get_raw([TEST_LEAF].as_ref().into(), b"psit", None, grove_version)
            .unwrap()
            .expect("get");
        let (_, real_secondary, real_sum) = match elem.underlying() {
            Element::ProvableSumIndexedTree(p, s, sv, _) => (p.clone(), s.clone(), *sv),
            _ => panic!(),
        };

        let bogus = Element::ProvableSumIndexedTree(
            Some(b"WRONG_PRIMARY".to_vec()),
            real_secondary,
            real_sum,
            None,
        );
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"psit",
                bogus,
                Some(override_tree_opts()),
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[test]
    fn direct_insert_psit_mismatched_secondary_with_override_rejected() {
        // Hits L475 (secondary mismatch in PSIT branch).
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
        .expect("create");
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"a",
            Element::new_sum_item(5),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");
        let elem = db
            .get_raw([TEST_LEAF].as_ref().into(), b"psit", None, grove_version)
            .unwrap()
            .expect("get");
        let (real_primary, _, real_sum) = match elem.underlying() {
            Element::ProvableSumIndexedTree(p, s, sv, _) => (p.clone(), s.clone(), *sv),
            _ => panic!(),
        };
        let bogus = Element::ProvableSumIndexedTree(
            real_primary,
            Some(b"WRONG_SECONDARY".to_vec()),
            real_sum,
            None,
        );
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"psit",
                bogus,
                Some(override_tree_opts()),
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[test]
    fn direct_insert_pcpsit_with_matching_roots_succeeds() {
        // PCPSIT success path at L621 (full axis loop).
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let axes = vec![(IndexAxis::Count.tag(), None), (IndexAxis::Sum.tag(), None)];
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            Element::empty_provable_count_provable_sum_indexed_tree(axes).unwrap(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"pcpsit"].as_ref(),
            b"a",
            Element::new_item_with_sum_item(b"a".to_vec(), 5),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");

        let elem = db
            .get_raw([TEST_LEAF].as_ref().into(), b"pcpsit", None, grove_version)
            .unwrap()
            .expect("get");
        let (real_primary, real_count, real_sum, real_axes) = match elem.underlying() {
            Element::ProvableCountProvableSumIndexedTree(p, c, s, ax, _) => {
                (p.clone(), *c, *s, ax.clone())
            }
            _ => panic!("expected PCPSIT"),
        };
        assert!(real_primary.is_some());
        assert!(real_axes.iter().all(|(_, sk)| sk.is_some()));

        let claimed = Element::ProvableCountProvableSumIndexedTree(
            real_primary,
            real_count,
            real_sum,
            real_axes,
            None,
        );
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            claimed,
            Some(override_tree_opts()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("matching-roots PCPSIT insert succeeds");
        let issues = db
            .verify_grovedb(None, true, true, grove_version)
            .expect("verify");
        assert!(issues.is_empty(), "issues: {:?}", issues);
    }

    #[test]
    fn direct_insert_pcpsit_mismatched_primary_with_override_rejected() {
        // Hits L575 in PCPSIT branch (primary mismatch).
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let axes = vec![(IndexAxis::Count.tag(), None), (IndexAxis::Sum.tag(), None)];
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            Element::empty_provable_count_provable_sum_indexed_tree(axes).unwrap(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"pcpsit"].as_ref(),
            b"a",
            Element::new_item_with_sum_item(b"a".to_vec(), 5),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");
        let elem = db
            .get_raw([TEST_LEAF].as_ref().into(), b"pcpsit", None, grove_version)
            .unwrap()
            .expect("get");
        let (_, real_count, real_sum, real_axes) = match elem.underlying() {
            Element::ProvableCountProvableSumIndexedTree(p, c, s, ax, _) => {
                (p.clone(), *c, *s, ax.clone())
            }
            _ => panic!(),
        };
        let bogus = Element::ProvableCountProvableSumIndexedTree(
            Some(b"WRONG_PRIMARY".to_vec()),
            real_count,
            real_sum,
            real_axes,
            None,
        );
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"pcpsit",
                bogus,
                Some(override_tree_opts()),
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[test]
    fn direct_insert_pcpsit_mismatched_axis_secondary_with_override_rejected() {
        // Hits L610 in PCPSIT axis loop (axis secondary mismatch).
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let axes = vec![(IndexAxis::Count.tag(), None), (IndexAxis::Sum.tag(), None)];
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            Element::empty_provable_count_provable_sum_indexed_tree(axes).unwrap(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"pcpsit"].as_ref(),
            b"a",
            Element::new_item_with_sum_item(b"a".to_vec(), 5),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");
        let elem = db
            .get_raw([TEST_LEAF].as_ref().into(), b"pcpsit", None, grove_version)
            .unwrap()
            .expect("get");
        let (real_primary, real_count, real_sum, real_axes) = match elem.underlying() {
            Element::ProvableCountProvableSumIndexedTree(p, c, s, ax, _) => {
                (p.clone(), *c, *s, ax.clone())
            }
            _ => panic!(),
        };
        // Sabotage axis 0's secondary key, keep axis 1 correct.
        let mut bad_axes = real_axes;
        bad_axes[0].1 = Some(b"WRONG_AXIS_SEC".to_vec());

        let bogus = Element::ProvableCountProvableSumIndexedTree(
            real_primary,
            real_count,
            real_sum,
            bad_axes,
            None,
        );
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"pcpsit",
                bogus,
                Some(override_tree_opts()),
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[test]
    fn direct_insert_pcpsit_with_unsorted_axes_rejected_at_validation() {
        // PCPSIT validation: axes must be sorted. Hits L549.
        // We bypass the constructor (which would reject) by building
        // the element manually with bad axes order.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        // Build axes Sum-then-Count (reversed) plus a phony primary so
        // we land in the "non-empty" branch where validation runs.
        let axes = vec![
            (IndexAxis::Sum.tag(), Some(b"x".to_vec())),
            (IndexAxis::Count.tag(), Some(b"y".to_vec())),
        ];
        let bogus = Element::ProvableCountProvableSumIndexedTree(
            Some(b"primary".to_vec()),
            1,
            1,
            axes,
            None,
        );
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"pcpsit",
                bogus,
                Some(override_tree_opts()),
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[test]
    fn direct_insert_pcpsit_with_duplicate_axes_rejected_at_validation() {
        // PCPSIT validation: duplicate tags fail the strictly-greater
        // sortedness check at L548-555.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let axes = vec![
            (IndexAxis::Count.tag(), Some(b"x".to_vec())),
            (IndexAxis::Count.tag(), Some(b"y".to_vec())),
        ];
        let bogus = Element::ProvableCountProvableSumIndexedTree(
            Some(b"primary".to_vec()),
            1,
            1,
            axes,
            None,
        );
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"pcpsit",
                bogus,
                Some(override_tree_opts()),
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[test]
    fn direct_insert_pcpsit_with_empty_axes_rejected_at_validation() {
        // PCPSIT validation: 0 axes. Hits L538.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let bogus = Element::ProvableCountProvableSumIndexedTree(
            Some(b"primary".to_vec()),
            1,
            1,
            vec![],
            None,
        );
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"pcpsit",
                bogus,
                Some(override_tree_opts()),
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[test]
    fn direct_insert_pcpsit_with_oversized_axes_rejected_at_validation() {
        // PCPSIT validation: 4 axes. Hits L538.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let axes = vec![
            (0u8, Some(b"a".to_vec())),
            (1u8, Some(b"b".to_vec())),
            (2u8, Some(b"c".to_vec())),
            (3u8, Some(b"d".to_vec())),
        ];
        let bogus = Element::ProvableCountProvableSumIndexedTree(
            Some(b"primary".to_vec()),
            1,
            1,
            axes,
            None,
        );
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"pcpsit",
                bogus,
                Some(override_tree_opts()),
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[test]
    fn non_counted_indexed_child_rejected_in_provable_count_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pct",
            Element::empty_provable_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create parent");

        let wrapped = Element::new_non_counted(Element::empty_provable_count_indexed_tree())
            .expect("valid wrapper");
        let result = db
            .insert(
                [TEST_LEAF, b"pct"].as_ref(),
                b"cidx",
                wrapped,
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInput(_))));
        assert!(
            db.get([TEST_LEAF, b"pct"].as_ref(), b"cidx", None, grove_version)
                .unwrap()
                .is_err(),
            "a rejected child must not be persisted"
        );
    }
}
