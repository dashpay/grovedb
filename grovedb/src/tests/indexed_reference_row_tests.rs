//! Focused corruption coverage for canonical indexed-secondary rows.
//!
//! Each test changes one part of the stored row and asserts that
//! `verify_grovedb` reports the corresponding sentinel. Keeping these cases
//! separate makes the operator-facing diagnostics part of the regression
//! surface, rather than only checking that some unspecified error occurred.

#[cfg(test)]
mod tests {
    use grovedb_element::{indexed::IndexAxis, reference_path::ReferencePathType};
    use grovedb_merk::element::{
        get::ElementFetchFromStorageExtensions, insert::ElementInsertToStorageExtensions,
    };
    use grovedb_path::SubtreePath;
    use grovedb_storage::{Storage, StorageBatch};
    use grovedb_version::version::GroveVersion;

    use crate::{
        operations::indexed_tree::make_axis_secondary_key,
        tests::{make_test_grovedb, TEST_LEAF},
        Element, GroveDb,
    };

    fn pcit_with_one_entry(grove_version: &GroveVersion) -> crate::tests::TempGroveDb {
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
        .expect("create PCIT");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert entry");
        db
    }

    /// Replace the canonical row while choosing the target value hash it is
    /// combined with. The honest primary hash isolates row-shape corruption;
    /// an explicit hash isolates stale commitment detection.
    fn overwrite_row(
        db: &GroveDb,
        secondary_key: &[u8],
        row: Element,
        target_hash: Option<[u8; 32]>,
        grove_version: &GroveVersion,
    ) {
        let tx = db.start_transaction();
        let batch = StorageBatch::new();
        let path_segments: [&[u8]; 2] = [TEST_LEAF, b"cidx".as_ref()];
        let path: SubtreePath<&[u8]> = (&path_segments).into();

        let secondary_root_key = {
            let parent_merk = db
                .open_transactional_merk_at_path(
                    [TEST_LEAF].as_ref().into(),
                    &tx,
                    Some(&batch),
                    grove_version,
                )
                .unwrap()
                .expect("open parent");
            let cidx = Element::get(&parent_merk, b"cidx", true, grove_version)
                .unwrap()
                .expect("cidx element");
            match cidx.underlying() {
                Element::ProvableCountIndexedTree(_, secondary_root, ..) => secondary_root.clone(),
                other => panic!("not a PCIT element: {other:?}"),
            }
        };

        {
            let mut secondary = db
                .open_indexed_secondary_at_path(
                    path,
                    IndexAxis::Count,
                    secondary_root_key,
                    &tx,
                    Some(&batch),
                    grove_version,
                )
                .unwrap()
                .expect("open secondary");
            let bind_to = target_hash.unwrap_or_else(|| {
                let primary = db
                    .open_transactional_merk_at_path(
                        [TEST_LEAF, b"cidx".as_ref()].as_ref().into(),
                        &tx,
                        Some(&batch),
                        grove_version,
                    )
                    .unwrap()
                    .expect("open primary");
                primary
                    .get_value_hash(
                        b"a",
                        true,
                        None::<&fn(&[u8], &GroveVersion) -> _>,
                        grove_version,
                    )
                    .unwrap()
                    .expect("read value hash")
                    .expect("entry present")
            });
            row.insert_reference(&mut secondary, secondary_key, bind_to, None, grove_version)
                .unwrap()
                .expect("write row");
        }

        db.db
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .expect("commit batch");
        tx.commit().expect("commit transaction");
    }

    fn sentinel_path(kind: &str) -> Vec<Vec<u8>> {
        vec![
            TEST_LEAF.to_vec(),
            b"cidx".to_vec(),
            format!("__cidx_{kind}__").into_bytes(),
            b"a".to_vec(),
        ]
    }

    fn assert_only_issue(db: &GroveDb, kind: &str, grove_version: &GroveVersion) {
        let issues = db.verify_grovedb(None, false, true, grove_version).unwrap();
        let want = sentinel_path(kind);
        // Rewriting the secondary directly deliberately leaves the parent
        // indexed element's recorded secondary root stale, so the general
        // verifier also reports the structural mismatch at `[.../cidx]`.
        // Among indexed-row diagnostics, however, this fixture must produce
        // exactly the sentinel named by the test.
        let row_issue_paths = issues
            .keys()
            .filter(|path| {
                path.get(2)
                    .is_some_and(|segment| segment.starts_with(b"__cidx_"))
            })
            .collect::<Vec<_>>();
        assert_eq!(
            row_issue_paths.len(),
            1,
            "expected exactly `{}`, got {:?}",
            String::from_utf8_lossy(&want[2]),
            row_issue_paths
                .iter()
                .map(|path| path
                    .iter()
                    .map(|segment| String::from_utf8_lossy(segment).to_string())
                    .collect::<Vec<_>>())
                .collect::<Vec<_>>()
        );
        assert!(
            issues.contains_key(&want),
            "expected `{}`, got {:?}",
            String::from_utf8_lossy(&want[2]),
            issues.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn healthy_tree_stores_a_canonical_reference_row() {
        let grove_version = GroveVersion::latest();
        let db = pcit_with_one_entry(grove_version);
        let issues = db.verify_grovedb(None, false, true, grove_version).unwrap();
        assert!(issues.is_empty(), "healthy tree reported: {issues:?}");

        let tx = db.start_transaction();
        let batch = StorageBatch::new();
        let path_segments: [&[u8]; 2] = [TEST_LEAF, b"cidx".as_ref()];
        let secondary_root_key = {
            let parent = db
                .open_transactional_merk_at_path(
                    [TEST_LEAF].as_ref().into(),
                    &tx,
                    Some(&batch),
                    grove_version,
                )
                .unwrap()
                .unwrap();
            match Element::get(&parent, b"cidx", true, grove_version)
                .unwrap()
                .unwrap()
                .underlying()
            {
                Element::ProvableCountIndexedTree(_, secondary_root, ..) => secondary_root.clone(),
                other => panic!("not a PCIT: {other:?}"),
            }
        };
        let secondary = db
            .open_indexed_secondary_at_path(
                (&path_segments).into(),
                IndexAxis::Count,
                secondary_root_key,
                &tx,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .unwrap();
        let key = make_axis_secondary_key(IndexAxis::Count, 1, 0, b"a");
        let row = Element::get(&secondary, key.as_slice(), true, grove_version)
            .unwrap()
            .expect("row present");
        assert_eq!(
            row,
            Element::new_reference_with_sum_item_with_hops(
                ReferencePathType::SiblingReference(b"a".to_vec()),
                Some(1),
                1,
            )
        );
    }

    #[test]
    fn legacy_placeholder_row_has_a_specific_sentinel() {
        let grove_version = GroveVersion::latest();
        let db = pcit_with_one_entry(grove_version);
        let key = make_axis_secondary_key(IndexAxis::Count, 1, 0, b"a");
        overwrite_row(&db, &key, Element::new_sum_item(1), None, grove_version);
        assert_only_issue(&db, "secondary_legacy_or_non_reference_row", grove_version);
    }

    #[test]
    fn wrong_reference_path_has_a_specific_sentinel() {
        let grove_version = GroveVersion::latest();
        let db = pcit_with_one_entry(grove_version);
        let key = make_axis_secondary_key(IndexAxis::Count, 1, 0, b"a");
        overwrite_row(
            &db,
            &key,
            Element::new_reference_with_sum_item_with_hops(
                ReferencePathType::SiblingReference(b"somewhere-else".to_vec()),
                Some(1),
                1,
            ),
            None,
            grove_version,
        );
        assert_only_issue(&db, "secondary_reference_path_mismatch", grove_version);
    }

    #[test]
    fn wrong_reference_hop_budget_has_a_specific_sentinel() {
        let grove_version = GroveVersion::latest();
        let db = pcit_with_one_entry(grove_version);
        let key = make_axis_secondary_key(IndexAxis::Count, 1, 0, b"a");
        overwrite_row(
            &db,
            &key,
            Element::new_reference_with_sum_item_with_hops(
                ReferencePathType::SiblingReference(b"a".to_vec()),
                Some(2),
                1,
            ),
            None,
            grove_version,
        );
        assert_only_issue(&db, "secondary_reference_hop_mismatch", grove_version);
    }

    #[test]
    fn wrong_reference_sum_has_a_specific_sentinel() {
        let grove_version = GroveVersion::latest();
        let db = pcit_with_one_entry(grove_version);
        let key = make_axis_secondary_key(IndexAxis::Count, 1, 0, b"a");
        overwrite_row(
            &db,
            &key,
            Element::new_reference_with_sum_item_with_hops(
                ReferencePathType::SiblingReference(b"a".to_vec()),
                Some(1),
                99,
            ),
            None,
            grove_version,
        );
        assert_only_issue(&db, "secondary_reference_sum_mismatch", grove_version);
    }

    #[test]
    fn stale_target_hash_has_a_specific_sentinel() {
        let grove_version = GroveVersion::latest();
        let db = pcit_with_one_entry(grove_version);
        let key = make_axis_secondary_key(IndexAxis::Count, 1, 0, b"a");
        let canonical = Element::new_reference_with_sum_item_with_hops(
            ReferencePathType::SiblingReference(b"a".to_vec()),
            Some(1),
            1,
        );
        overwrite_row(&db, &key, canonical, Some([0xEE; 32]), grove_version);
        assert_only_issue(&db, "secondary_stale_target_hash", grove_version);
    }
}
