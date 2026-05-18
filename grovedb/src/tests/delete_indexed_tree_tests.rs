//! Coverage tests for the `delete_internal_on_transaction` indexed-tree
//! branches in `grovedb/src/operations/delete/mod.rs`.
//!
//! Targets:
//! - Deleting a PCIT/PSIT/PCPSIT primary with `allow_deleting_non_empty_trees`
//!   when the primary holds children — exercises the nested
//!   secondary-namespace sweep for all three axis tags.
//! - Deleting a PCIT/PSIT/PCPSIT primary with default options when the
//!   primary holds children — the "non-empty tree" guard fires and
//!   returns an error / false.
//! - Deleting an empty PCIT/PSIT/PCPSIT primary still triggers the
//!   defensive secondary clear.

#[cfg(test)]
mod tests {
    use grovedb_element::indexed::IndexAxis;
    use grovedb_version::version::GroveVersion;

    use crate::{
        operations::delete::DeleteOptions,
        tests::{make_test_grovedb, TEST_LEAF},
        Element, Error,
    };

    fn delete_options_allow_non_empty() -> DeleteOptions {
        DeleteOptions {
            allow_deleting_non_empty_trees: true,
            deleting_non_empty_trees_returns_error: false,
            base_root_storage_is_free: true,
            validate_tree_at_path_exists: false,
        }
    }

    // ---------- PCIT delete ----------

    #[test]
    fn delete_pcit_with_children_succeeds_with_allow_flag() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcit",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PCIT");
        for (k, c) in &[(b"a" as &[u8], 1u64), (b"b" as &[u8], 5u64)] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"pcit"].as_ref(),
                k,
                Element::new_provable_count_tree_with_flags_and_count_value(None, *c, None),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert PCIT entry");
        }
        // Delete the PCIT primary itself with allow flag.
        db.delete(
            [TEST_LEAF].as_ref(),
            b"pcit",
            Some(delete_options_allow_non_empty()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete PCIT root with children");
        // The PCIT element should be gone.
        let get_result = db
            .get([TEST_LEAF].as_ref(), b"pcit", None, grove_version)
            .unwrap();
        assert!(get_result.is_err());
    }

    #[test]
    fn delete_empty_pcit_succeeds_and_clears_secondary() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcit_empty",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create empty PCIT");
        // Delete with default options (no allow flag needed; tree is empty).
        db.delete(
            [TEST_LEAF].as_ref(),
            b"pcit_empty",
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete empty PCIT");
    }

    #[test]
    fn delete_pcit_non_empty_without_allow_returns_error() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcit",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PCIT");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"pcit"].as_ref(),
            b"a",
            Element::new_provable_count_tree_with_flags_and_count_value(None, 1, None),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert PCIT entry");
        let result = db
            .delete([TEST_LEAF].as_ref(), b"pcit", None, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::DeletingNonEmptyTree(_))));
    }

    // ---------- PSIT delete ----------

    #[test]
    fn delete_psit_with_children_succeeds_with_allow_flag() {
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
        .expect("create PSIT");
        for (k, s) in &[(b"a" as &[u8], 1i64), (b"b" as &[u8], -5i64)] {
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
        // Delete the PSIT primary with children.
        db.delete(
            [TEST_LEAF].as_ref(),
            b"psit",
            Some(delete_options_allow_non_empty()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete PSIT root with children");
        let get_result = db
            .get([TEST_LEAF].as_ref(), b"psit", None, grove_version)
            .unwrap();
        assert!(get_result.is_err());
    }

    #[test]
    fn delete_empty_psit_succeeds() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"psit_empty",
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create empty PSIT");
        db.delete(
            [TEST_LEAF].as_ref(),
            b"psit_empty",
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete empty PSIT");
    }

    #[test]
    fn delete_psit_non_empty_without_allow_returns_error() {
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
        .expect("create PSIT");
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"a",
            Element::new_sum_item(7),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert PSIT entry");
        let result = db
            .delete([TEST_LEAF].as_ref(), b"psit", None, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::DeletingNonEmptyTree(_))));
    }

    // ---------- PCPSIT delete ----------

    #[test]
    fn delete_pcpsit_with_multi_axis_children_succeeds_with_allow_flag() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let axes: Vec<(u8, Option<Vec<u8>>)> = vec![
            (IndexAxis::Count.tag(), None),
            (IndexAxis::Sum.tag(), None),
            (IndexAxis::Avg.tag(), None),
        ];
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
        for (k, s) in &[
            (b"a" as &[u8], 1i64),
            (b"b" as &[u8], 5i64),
            (b"c" as &[u8], 9),
        ] {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                k,
                Element::new_item_with_sum_item(b"v".to_vec(), *s),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert PCPSIT entry");
        }
        // Delete the PCPSIT primary with children — exercises the
        // multi-axis secondary cleanup sweep over all three IndexAxis
        // tags inside the find_subtrees loop.
        db.delete(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            Some(delete_options_allow_non_empty()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete PCPSIT root with multi-axis children");
        let get_result = db
            .get([TEST_LEAF].as_ref(), b"pcpsit", None, grove_version)
            .unwrap();
        assert!(get_result.is_err());
    }

    #[test]
    fn delete_pcpsit_single_axis_with_children_succeeds_with_allow_flag() {
        // PCPSIT with only the count axis in TLV; delete with children.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let axes: Vec<(u8, Option<Vec<u8>>)> = vec![(IndexAxis::Count.tag(), None)];
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
        for k in &[b"a" as &[u8], b"b", b"c"] {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                k,
                Element::new_item_with_sum_item(b"v".to_vec(), 1),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert PCPSIT entry");
        }
        db.delete(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            Some(delete_options_allow_non_empty()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete PCPSIT root with single-axis children");
    }

    #[test]
    fn delete_empty_pcpsit_succeeds_with_default_options() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let axes: Vec<(u8, Option<Vec<u8>>)> = vec![(IndexAxis::Sum.tag(), None)];
        let elem =
            Element::empty_provable_count_provable_sum_indexed_tree(axes).expect("axes canonical");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit_empty",
            elem,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create empty PCPSIT");
        db.delete(
            [TEST_LEAF].as_ref(),
            b"pcpsit_empty",
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete empty PCPSIT");
    }

    #[test]
    fn delete_pcpsit_non_empty_without_allow_returns_error() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let axes: Vec<(u8, Option<Vec<u8>>)> =
            vec![(IndexAxis::Count.tag(), None), (IndexAxis::Sum.tag(), None)];
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
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"pcpsit"].as_ref(),
            b"a",
            Element::new_item_with_sum_item(b"v".to_vec(), 1),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert PCPSIT entry");
        let result = db
            .delete([TEST_LEAF].as_ref(), b"pcpsit", None, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::DeletingNonEmptyTree(_))));
    }

    // ---------- Nested indexed-tree delete ----------

    #[test]
    fn delete_outer_tree_with_nested_pcpsit_clears_secondaries() {
        // Outer regular tree contains a nested PCPSIT with children.
        // Deleting the outer tree must sweep the nested PCPSIT's three
        // secondary namespaces via the per-prefix axis sweep.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create outer");
        let axes: Vec<(u8, Option<Vec<u8>>)> = vec![
            (IndexAxis::Count.tag(), None),
            (IndexAxis::Sum.tag(), None),
            (IndexAxis::Avg.tag(), None),
        ];
        let elem =
            Element::empty_provable_count_provable_sum_indexed_tree(axes).expect("axes canonical");
        db.insert(
            [TEST_LEAF, b"outer"].as_ref(),
            b"nested_pcpsit",
            elem,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create nested PCPSIT");
        for k in &[b"a" as &[u8], b"b"] {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"outer", b"nested_pcpsit"].as_ref(),
                k,
                Element::new_item_with_sum_item(b"v".to_vec(), 1),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert nested PCPSIT entry");
        }
        // Delete the outer tree with allow flag — exercise the
        // find_subtrees walk + the nested-secondary sweep over all
        // three axis tags for the nested PCPSIT.
        db.delete(
            [TEST_LEAF].as_ref(),
            b"outer",
            Some(delete_options_allow_non_empty()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete outer tree containing nested PCPSIT");
    }

    #[test]
    fn delete_outer_tree_with_nested_psit_clears_secondaries() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create outer");
        db.insert(
            [TEST_LEAF, b"outer"].as_ref(),
            b"nested_psit",
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create nested PSIT");
        for (k, s) in &[(b"a" as &[u8], 1i64), (b"b" as &[u8], 2i64)] {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"outer", b"nested_psit"].as_ref(),
                k,
                Element::new_sum_item(*s),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert nested PSIT entry");
        }
        db.delete(
            [TEST_LEAF].as_ref(),
            b"outer",
            Some(delete_options_allow_non_empty()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete outer tree containing nested PSIT");
    }
}
