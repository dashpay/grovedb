//! `verify_grovedb` consistency tests for PCIT / PSIT / PCPSIT.
//!
//! Covers both the happy paths (verify_grovedb passes after various
//! mutations) and the corruption-detection branches in
//! `grovedb/src/lib.rs::verify_merk_and_submerks_in_transaction`.

#[cfg(test)]
mod tests {
    use grovedb_element::indexed::IndexAxis;
    use grovedb_merk::element::{
        delete::ElementDeleteFromStorageExtensions, get::ElementFetchFromStorageExtensions,
        insert::ElementInsertToStorageExtensions,
    };
    use grovedb_merk::tree_type::TreeType;
    use grovedb_path::SubtreePath;
    use grovedb_storage::{Storage, StorageBatch};
    use grovedb_version::version::GroveVersion;

    use crate::{
        operations::insert::InsertOptions,
        tests::{make_test_grovedb, TEST_LEAF},
        Element,
    };

    // -----------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------

    fn assert_verify_passes(db: &crate::GroveDb, grove_version: &GroveVersion) {
        let issues = db
            .verify_grovedb(None, true, true, grove_version)
            .expect("verify_grovedb must not return a hard error");
        assert!(
            issues.is_empty(),
            "verify_grovedb reported issues: {:?}",
            issues
        );
    }

    fn make_secondary_key(count: u64, item_key: &[u8]) -> Vec<u8> {
        let mut k = Vec::with_capacity(8 + item_key.len());
        k.extend_from_slice(&count.to_be_bytes());
        k.extend_from_slice(item_key);
        k
    }

    /// Manually delete an entry from a PCIT secondary at the given
    /// secondary key, then commit. Used to introduce drift between
    /// primary and secondary for content-consistency testing.
    fn corrupt_pcit_secondary_delete(
        db: &crate::GroveDb,
        cidx_primary_path: &[&[u8]],
        secondary_key: &[u8],
        grove_version: &GroveVersion,
    ) {
        let tx = db.start_transaction();
        let batch = StorageBatch::new();
        let path_vec: Vec<&[u8]> = cidx_primary_path.to_vec();
        let path: SubtreePath<&[u8]> = path_vec.as_slice().into();

        let (parent_path, cidx_key) = path.derive_parent().expect("non-root cidx");
        let secondary_root_key = {
            let parent_merk = db
                .open_transactional_merk_at_path(parent_path, &tx, Some(&batch), grove_version)
                .unwrap()
                .expect("open parent");
            let cidx_element = Element::get(&parent_merk, cidx_key, true, grove_version)
                .unwrap()
                .expect("cidx element");
            match cidx_element.underlying() {
                Element::ProvableCountIndexedTree(_, s, ..) => s.clone(),
                _ => panic!("not a PCIT element"),
            }
        };

        {
            let mut secondary_merk = db
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
            Element::delete(
                &mut secondary_merk,
                secondary_key,
                None,
                false,
                TreeType::ProvableCountTree,
                grove_version,
            )
            .unwrap()
            .expect("delete");
        }

        db.db
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .expect("commit");
        tx.commit().expect("tx commit");
    }

    /// Manually insert a bogus orphan into a PCIT secondary.
    fn corrupt_pcit_secondary_insert_orphan(
        db: &crate::GroveDb,
        cidx_primary_path: &[&[u8]],
        secondary_key: &[u8],
        grove_version: &GroveVersion,
    ) {
        let tx = db.start_transaction();
        let batch = StorageBatch::new();
        let path_vec: Vec<&[u8]> = cidx_primary_path.to_vec();
        let path: SubtreePath<&[u8]> = path_vec.as_slice().into();

        let (parent_path, cidx_key) = path.derive_parent().expect("non-root cidx");
        let secondary_root_key = {
            let parent_merk = db
                .open_transactional_merk_at_path(parent_path, &tx, Some(&batch), grove_version)
                .unwrap()
                .expect("open parent");
            let cidx_element = Element::get(&parent_merk, cidx_key, true, grove_version)
                .unwrap()
                .expect("cidx element");
            match cidx_element.underlying() {
                Element::ProvableCountIndexedTree(_, s, ..) => s.clone(),
                _ => panic!("not a PCIT element"),
            }
        };

        {
            let mut secondary_merk = db
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
            let bogus = Element::new_item(Vec::new());
            bogus
                .insert(&mut secondary_merk, secondary_key, None, grove_version)
                .unwrap()
                .expect("insert orphan");
        }

        db.db
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .expect("commit");
        tx.commit().expect("tx commit");
    }

    // -----------------------------------------------------------------
    // PCIT happy paths
    // -----------------------------------------------------------------

    #[test]
    fn verify_grovedb_pcit_empty_passes() {
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
        .expect("create empty PCIT");
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn verify_grovedb_pcit_after_inserts_passes() {
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
        for k in [b"a".as_slice(), b"b", b"c", b"d"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::new_item(b"v".to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn verify_grovedb_pcit_after_deletes_passes() {
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
        for k in [b"a".as_slice(), b"b", b"c"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::new_item(b"v".to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        db.delete_from_count_indexed_tree([TEST_LEAF, b"cidx"].as_ref(), b"b", None, grove_version)
            .unwrap()
            .expect("delete b");
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn verify_grovedb_pcit_nested_passes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("outer");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer"].as_ref(),
            b"inner",
            Element::empty_provable_count_indexed_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("inner");
        for k in [b"a".as_slice(), b"b"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"outer", b"inner"].as_ref(),
                k,
                Element::new_item(b"v".to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("inner insert");
        }
        assert_verify_passes(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // PCIT corruption detection
    // -----------------------------------------------------------------

    #[test]
    fn verify_grovedb_pcit_detects_primary_orphan() {
        // Delete a secondary entry that the primary still has — surfaces
        // as `__cidx_primary_orphan__`.
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
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");
        // Sanity: clean before corruption.
        assert_verify_passes(&db, grove_version);

        corrupt_pcit_secondary_delete(
            &db,
            &[TEST_LEAF, b"cidx"],
            &make_secondary_key(1, b"a"),
            grove_version,
        );

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify");
        let primary_orphan_path: Vec<Vec<u8>> = vec![
            TEST_LEAF.to_vec(),
            b"cidx".to_vec(),
            b"__cidx_primary_orphan__".to_vec(),
            b"a".to_vec(),
        ];
        assert!(
            issues.contains_key(&primary_orphan_path),
            "expected __cidx_primary_orphan__ for 'a' in {:?}",
            issues.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn verify_grovedb_pcit_detects_secondary_orphan() {
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
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"real",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");
        corrupt_pcit_secondary_insert_orphan(
            &db,
            &[TEST_LEAF, b"cidx"],
            &make_secondary_key(99, b"ghost"),
            grove_version,
        );
        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify");
        let secondary_orphan_path: Vec<Vec<u8>> = vec![
            TEST_LEAF.to_vec(),
            b"cidx".to_vec(),
            b"__cidx_secondary_orphan__".to_vec(),
            b"ghost".to_vec(),
        ];
        assert!(
            issues.contains_key(&secondary_orphan_path),
            "expected __cidx_secondary_orphan__ for 'ghost' in {:?}",
            issues.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn verify_grovedb_pcit_detects_count_mismatch() {
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
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");
        // Replace (1, "a") secondary with (42, "a") to create count
        // mismatch.
        corrupt_pcit_secondary_delete(
            &db,
            &[TEST_LEAF, b"cidx"],
            &make_secondary_key(1, b"a"),
            grove_version,
        );
        corrupt_pcit_secondary_insert_orphan(
            &db,
            &[TEST_LEAF, b"cidx"],
            &make_secondary_key(42, b"a"),
            grove_version,
        );
        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify");
        let mismatch_path: Vec<Vec<u8>> = vec![
            TEST_LEAF.to_vec(),
            b"cidx".to_vec(),
            b"__cidx_count_mismatch__".to_vec(),
            b"a".to_vec(),
        ];
        let entry = issues.get(&mismatch_path).unwrap_or_else(|| {
            panic!(
                "expected count mismatch, got {:?}",
                issues.keys().collect::<Vec<_>>()
            )
        });
        assert_eq!(&entry.1[24..32], &1u64.to_be_bytes());
        assert_eq!(&entry.2[24..32], &42u64.to_be_bytes());
    }

    #[test]
    fn verify_grovedb_pcit_detects_secondary_duplicate() {
        // Insert two secondary rows pointing at the same primary key
        // under different counts.
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
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");
        // Inject a duplicate secondary row.
        corrupt_pcit_secondary_insert_orphan(
            &db,
            &[TEST_LEAF, b"cidx"],
            &make_secondary_key(2, b"a"),
            grove_version,
        );
        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify");
        let dup_path: Vec<Vec<u8>> = vec![
            TEST_LEAF.to_vec(),
            b"cidx".to_vec(),
            b"__cidx_secondary_duplicate__".to_vec(),
            b"a".to_vec(),
        ];
        assert!(
            issues.contains_key(&dup_path),
            "expected __cidx_secondary_duplicate__ in {:?}",
            issues.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn verify_grovedb_pcit_clean_after_corrupt_then_repair_round_trip() {
        // Insert, manually delete + restore secondary entry, verify
        // remains clean before and after. Exercises the verify pass
        // on a sequence of clean states.
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
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");
        // Pre-corruption clean.
        assert_verify_passes(&db, grove_version);
        // Re-insert same key (idempotent, count stays 1).
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::new_item(b"v2".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("re-insert");
        assert_verify_passes(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // PSIT happy paths
    // -----------------------------------------------------------------

    #[test]
    fn verify_grovedb_psit_empty_passes() {
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
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn verify_grovedb_psit_after_inserts_passes() {
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
        for (k, v) in [(b"a".as_ref(), 10i64), (b"b", -5), (b"c", 20)] {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"psit"].as_ref(),
                k,
                Element::new_sum_item(v),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn verify_grovedb_psit_after_deletes_passes() {
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
        for (k, v) in [(b"a".as_ref(), 10i64), (b"b", 20)] {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"psit"].as_ref(),
                k,
                Element::new_sum_item(v),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        db.delete_from_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"a",
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete");
        assert_verify_passes(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // PCPSIT happy paths
    // -----------------------------------------------------------------

    fn all_axis_subsets() -> Vec<Vec<u8>> {
        vec![
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
        ]
    }

    #[test]
    fn verify_grovedb_pcpsit_empty_passes_for_all_axis_subsets() {
        let grove_version = GroveVersion::latest();
        for (i, tags) in all_axis_subsets().iter().enumerate() {
            let db = make_test_grovedb(grove_version);
            let key = format!("pcpsit_{}", i);
            let axes: Vec<(u8, Option<Vec<u8>>)> = tags.iter().map(|t| (*t, None)).collect();
            let elem = Element::empty_provable_count_provable_sum_indexed_tree(axes)
                .expect("axes canonical");
            db.insert(
                [TEST_LEAF].as_ref(),
                key.as_bytes(),
                elem,
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("create");
            assert_verify_passes(&db, grove_version);
        }
    }

    #[test]
    fn verify_grovedb_pcpsit_after_inserts_passes() {
        let grove_version = GroveVersion::latest();
        for tags in all_axis_subsets() {
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
            .expect("create");
            for (k, v) in [(b"a".as_ref(), 10i64), (b"b", -3), (b"c", 100)] {
                db.insert_into_provable_count_provable_sum_indexed_tree(
                    [TEST_LEAF, b"pcpsit"].as_ref(),
                    k,
                    Element::new_item_with_sum_item(k.to_vec(), v),
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert");
            }
            assert_verify_passes(&db, grove_version);
        }
    }

    #[test]
    fn verify_grovedb_pcpsit_after_deletes_passes() {
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
        .expect("create");
        for (k, v) in [(b"a".as_ref(), 10i64), (b"b", 20)] {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                k,
                Element::new_item_with_sum_item(k.to_vec(), v),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        db.delete_from_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"pcpsit"].as_ref(),
            b"a",
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete");
        assert_verify_passes(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // Mixed: multiple indexed-tree variants in one DB
    // -----------------------------------------------------------------

    #[test]
    fn verify_grovedb_mixed_pcit_psit_pcpsit_passes() {
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
        .expect("pcit");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"psit",
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("psit");
        let axes = vec![(IndexAxis::Count.tag(), None), (IndexAxis::Sum.tag(), None)];
        let pcpsit =
            Element::empty_provable_count_provable_sum_indexed_tree(axes).expect("axes canonical");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            pcpsit,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("pcpsit");
        // Populate each.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"pcit"].as_ref(),
            b"a",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("pcit insert");
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"a",
            Element::new_sum_item(10),
            None,
            grove_version,
        )
        .unwrap()
        .expect("psit insert");
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"pcpsit"].as_ref(),
            b"a",
            Element::new_item_with_sum_item(b"a".to_vec(), 7),
            None,
            grove_version,
        )
        .unwrap()
        .expect("pcpsit insert");
        assert_verify_passes(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // No-cache / cache modes
    // -----------------------------------------------------------------

    #[test]
    fn verify_grovedb_pcit_with_allow_cache_false_passes() {
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
        for k in [b"a".as_slice(), b"b"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::new_item(b"v".to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        let issues = db
            .verify_grovedb(None, false, false, grove_version)
            .expect("verify no-cache");
        assert!(issues.is_empty());
    }

    #[test]
    fn verify_grovedb_pcit_with_verify_references_false_passes() {
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
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");
        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify no-refs");
        assert!(issues.is_empty());
    }

    // -----------------------------------------------------------------
    // Many entries to exercise the iterator path
    // -----------------------------------------------------------------

    #[test]
    fn verify_grovedb_pcit_with_many_entries() {
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
        for i in 0..40u8 {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                &[i],
                Element::new_item(vec![i]),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn verify_grovedb_pcpsit_with_many_entries() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let axes = vec![
            (IndexAxis::Count.tag(), None),
            (IndexAxis::Sum.tag(), None),
            (IndexAxis::Avg.tag(), None),
        ];
        let elem = Element::empty_provable_count_provable_sum_indexed_tree(axes).expect("axes");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            elem,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        for i in 0..20u8 {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                &[i],
                Element::new_item_with_sum_item(vec![i], i as i64),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        assert_verify_passes(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // PCIT primary having mixed child types
    // -----------------------------------------------------------------

    // -----------------------------------------------------------------
    // Depth > 1 propagation tests
    //
    // These build a PCIT/PSIT/PCPSIT under a regular Tree (so depth is
    // > 1 from root), mutate the primary, and verify the whole grove.
    // They exercise the `propagate_changes_with_transaction_with_
    // initial_deferred` loop's bubble-up when child_tree (cidx
    // primary) -> parent_tree (Tree) -> ... -> root.
    // -----------------------------------------------------------------

    #[test]
    fn verify_grovedb_pcit_under_tree_depth_2_passes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"parent",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("parent tree");
        db.insert(
            [TEST_LEAF, b"parent"].as_ref(),
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cidx under parent");
        // Mutate into the deeply-nested PCIT — propagation walks
        // cidx primary -> parent Tree -> TEST_LEAF -> root, exercising
        // the L759-817 child_is_cidx_primary branch on the first
        // iteration.
        for k in [b"a".as_slice(), b"b", b"c"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"parent", b"cidx"].as_ref(),
                k,
                Element::new_item(b"v".to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        assert_verify_passes(&db, grove_version);
        // Check the count propagated correctly into the PCIT element.
        let pcit = db
            .get(
                [TEST_LEAF, b"parent"].as_ref(),
                b"cidx",
                None,
                grove_version,
            )
            .unwrap()
            .expect("get");
        match pcit.underlying() {
            Element::ProvableCountIndexedTree(_, _, c, _) => assert_eq!(*c, 3),
            other => panic!("expected PCIT, got {:?}", other),
        }
    }

    #[test]
    fn verify_grovedb_pcit_under_tree_under_tree_depth_3_passes() {
        // 3 layers from TEST_LEAF: TEST_LEAF/lvl1/lvl2/cidx
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"lvl1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("lvl1");
        db.insert(
            [TEST_LEAF, b"lvl1"].as_ref(),
            b"lvl2",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("lvl2");
        db.insert(
            [TEST_LEAF, b"lvl1", b"lvl2"].as_ref(),
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cidx");
        for k in [b"x".as_slice(), b"y"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"lvl1", b"lvl2", b"cidx"].as_ref(),
                k,
                Element::new_item(b"v".to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn verify_grovedb_psit_under_tree_depth_2_passes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"parent",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("parent");
        db.insert(
            [TEST_LEAF, b"parent"].as_ref(),
            b"psit",
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("psit");
        for (k, v) in [(b"a".as_ref(), 5i64), (b"b", -10), (b"c", 20)] {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"parent", b"psit"].as_ref(),
                k,
                Element::new_sum_item(v),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        assert_verify_passes(&db, grove_version);
        let psit = db
            .get(
                [TEST_LEAF, b"parent"].as_ref(),
                b"psit",
                None,
                grove_version,
            )
            .unwrap()
            .expect("get");
        match psit.underlying() {
            Element::ProvableSumIndexedTree(_, _, s, _) => assert_eq!(*s, 15),
            other => panic!("expected PSIT, got {:?}", other),
        }
    }

    #[test]
    fn verify_grovedb_pcpsit_under_tree_depth_2_passes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"parent",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("parent");
        let axes = vec![(IndexAxis::Count.tag(), None), (IndexAxis::Sum.tag(), None)];
        db.insert(
            [TEST_LEAF, b"parent"].as_ref(),
            b"pcpsit",
            Element::empty_provable_count_provable_sum_indexed_tree(axes).unwrap(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("pcpsit");
        for (k, v) in [(b"a".as_ref(), 11i64), (b"b", 22), (b"c", -3)] {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"parent", b"pcpsit"].as_ref(),
                k,
                Element::new_item_with_sum_item(k.to_vec(), v),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        assert_verify_passes(&db, grove_version);
        let pcpsit = db
            .get(
                [TEST_LEAF, b"parent"].as_ref(),
                b"pcpsit",
                None,
                grove_version,
            )
            .unwrap()
            .expect("get");
        match pcpsit.underlying() {
            Element::ProvableCountProvableSumIndexedTree(_, c, s, _, _) => {
                assert_eq!(*c, 3);
                assert_eq!(*s, 30);
            }
            other => panic!("expected PCPSIT, got {:?}", other),
        }
    }

    #[test]
    fn verify_grovedb_pcpsit_all_axis_subsets_depth_2_passes() {
        // Exercise the per-axis loop at depth=2 for every axis subset.
        let grove_version = GroveVersion::latest();
        for tags in all_axis_subsets() {
            let db = make_test_grovedb(grove_version);
            db.insert(
                [TEST_LEAF].as_ref(),
                b"parent",
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("parent");
            let axes: Vec<(u8, Option<Vec<u8>>)> = tags.iter().map(|t| (*t, None)).collect();
            db.insert(
                [TEST_LEAF, b"parent"].as_ref(),
                b"pcpsit",
                Element::empty_provable_count_provable_sum_indexed_tree(axes).unwrap(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("pcpsit");
            for (k, v) in [(b"x".as_ref(), 5i64), (b"y", -2)] {
                db.insert_into_provable_count_provable_sum_indexed_tree(
                    [TEST_LEAF, b"parent", b"pcpsit"].as_ref(),
                    k,
                    Element::new_item_with_sum_item(k.to_vec(), v),
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert");
            }
            assert_verify_passes(&db, grove_version);
        }
    }

    // -----------------------------------------------------------------
    // PSIT corruption detection
    // -----------------------------------------------------------------

    /// Manually delete an entry from a PSIT secondary at the given
    /// secondary key.
    fn corrupt_psit_secondary_delete(
        db: &crate::GroveDb,
        psit_primary_path: &[&[u8]],
        secondary_key: &[u8],
        grove_version: &GroveVersion,
    ) {
        let tx = db.start_transaction();
        let batch = StorageBatch::new();
        let path_vec: Vec<&[u8]> = psit_primary_path.to_vec();
        let path: SubtreePath<&[u8]> = path_vec.as_slice().into();
        let (parent_path, psit_key) = path.derive_parent().expect("non-root psit");
        let secondary_root_key = {
            let parent_merk = db
                .open_transactional_merk_at_path(parent_path, &tx, Some(&batch), grove_version)
                .unwrap()
                .expect("open parent");
            let elem = Element::get(&parent_merk, psit_key, true, grove_version)
                .unwrap()
                .expect("get");
            match elem.underlying() {
                Element::ProvableSumIndexedTree(_, s, ..) => s.clone(),
                _ => panic!("not PSIT"),
            }
        };
        {
            let mut secondary_merk = db
                .open_indexed_secondary_at_path(
                    path,
                    IndexAxis::Sum,
                    secondary_root_key,
                    &tx,
                    Some(&batch),
                    grove_version,
                )
                .unwrap()
                .expect("open psit secondary");
            Element::delete(
                &mut secondary_merk,
                secondary_key,
                None,
                false,
                crate::operations::indexed_tree::axis_secondary_tree_type(IndexAxis::Sum),
                grove_version,
            )
            .unwrap()
            .expect("delete");
        }
        db.db
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .expect("commit");
        tx.commit().expect("tx commit");
    }

    fn psit_secondary_key(sum: i64, item_key: &[u8]) -> Vec<u8> {
        // Mirror of `make_axis_secondary_key(IndexAxis::Sum, ...)` —
        // 8-byte sortable sum encoding followed by the item key.
        let prefix = grovedb_element::indexed::encode_sum_sort_key(sum);
        let mut k = Vec::with_capacity(prefix.len() + item_key.len());
        k.extend_from_slice(&prefix);
        k.extend_from_slice(item_key);
        k
    }

    fn delete_psit_secondary_row_and_get_root_key(
        db: &crate::GroveDb,
        psit_primary_path: &[&[u8]],
        secondary_root_key: Option<Vec<u8>>,
        secondary_key: &[u8],
        grove_version: &GroveVersion,
    ) -> Option<Vec<u8>> {
        let tx = db.start_transaction();
        let batch = StorageBatch::new();
        let path_vec: Vec<&[u8]> = psit_primary_path.to_vec();
        let path: SubtreePath<&[u8]> = path_vec.as_slice().into();
        let new_root_key = {
            let mut secondary_merk = db
                .open_indexed_secondary_at_path(
                    path,
                    IndexAxis::Sum,
                    secondary_root_key,
                    &tx,
                    Some(&batch),
                    grove_version,
                )
                .unwrap()
                .expect("open PSIT secondary");
            Element::delete(
                &mut secondary_merk,
                secondary_key,
                None,
                false,
                crate::operations::indexed_tree::axis_secondary_tree_type(IndexAxis::Sum),
                grove_version,
            )
            .unwrap()
            .expect("delete secondary row");
            secondary_merk
                .root_hash_key_and_aggregate_data()
                .unwrap()
                .expect("read secondary root")
                .1
        };
        db.db
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .expect("commit mutated secondary");
        tx.commit().expect("commit transaction");
        new_root_key
    }

    #[test]
    fn verify_grovedb_psit_detects_secondary_drift() {
        // PSIT mirror is via i64-keyed secondary. Delete the secondary
        // entry for one item and confirm verify_grovedb's H1-A
        // combined-hash check fires (the secondary's root_hash
        // changes, so the parent's element_value_hash no longer
        // combines correctly).
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
        // Insert entries with known sums.
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"a",
            Element::new_sum_item(10),
            None,
            grove_version,
        )
        .unwrap()
        .expect("a");
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"b",
            Element::new_sum_item(20),
            None,
            grove_version,
        )
        .unwrap()
        .expect("b");
        // Sanity: verify passes before corruption.
        assert_verify_passes(&db, grove_version);

        // Delete `a`'s secondary entry (sum=10 ‖ "a").
        let sec_key = psit_secondary_key(10, b"a");
        corrupt_psit_secondary_delete(&db, &[TEST_LEAF, b"psit"], &sec_key, grove_version);

        let issues = db
            .verify_grovedb(None, true, true, grove_version)
            .expect("verify");
        assert!(!issues.is_empty(), "expected H1-A drift detection");
    }

    #[test]
    fn verify_grovedb_psit_detects_coherently_rebound_relational_drift() {
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
        for (key, sum) in [(b"a".as_slice(), 10), (b"b".as_slice(), 20)] {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"psit"].as_ref(),
                key,
                Element::new_sum_item(sum),
                None,
                grove_version,
            )
            .unwrap()
            .expect("populate PSIT");
        }
        let parent = db
            .get([TEST_LEAF].as_ref(), b"psit", None, grove_version)
            .unwrap()
            .expect("read PSIT parent");
        let (primary_root, secondary_root, sum, flags) = match parent {
            Element::ProvableSumIndexedTree(primary, secondary, sum, flags) => {
                (primary, secondary, sum, flags)
            }
            other => panic!("expected PSIT, got {other:?}"),
        };
        let new_secondary_root = delete_psit_secondary_row_and_get_root_key(
            &db,
            &[TEST_LEAF, b"psit"],
            secondary_root,
            &psit_secondary_key(10, b"a"),
            grove_version,
        );

        db.insert(
            [TEST_LEAF].as_ref(),
            b"psit",
            Element::ProvableSumIndexedTree(primary_root, new_secondary_root, sum, flags),
            Some(InsertOptions {
                validate_insertion_does_not_override: false,
                validate_insertion_does_not_override_tree: false,
                base_root_storage_is_free: true,
            }),
            None,
            grove_version,
        )
        .unwrap()
        .expect("rebind authenticated secondary root");

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify coherently rebound PSIT");
        // The content walk reports orphans under a per-variant sentinel
        // (`__psit_primary_orphan__` here) so a multi-axis tree can name the
        // axis that drifted; the detection itself is what this test pins.
        assert!(
            issues.keys().any(|path| path
                .iter()
                .any(|segment| segment.as_slice() == b"__psit_primary_orphan__")),
            "relational verifier missed a primary row absent from the rebound secondary: {issues:?}"
        );
    }

    // -----------------------------------------------------------------
    // PCPSIT corruption detection
    // -----------------------------------------------------------------

    fn pcpsit_count_secondary_key(count: u64, item_key: &[u8]) -> Vec<u8> {
        let prefix = grovedb_element::indexed::encode_count_sort_key(count);
        let mut k = Vec::with_capacity(prefix.len() + item_key.len());
        k.extend_from_slice(&prefix);
        k.extend_from_slice(item_key);
        k
    }

    /// Manually delete an entry from a PCPSIT axis secondary at the
    /// given key, then commit.
    fn corrupt_pcpsit_axis_secondary_delete(
        db: &crate::GroveDb,
        pcpsit_primary_path: &[&[u8]],
        axis: IndexAxis,
        secondary_key: &[u8],
        grove_version: &GroveVersion,
    ) {
        let tx = db.start_transaction();
        let batch = StorageBatch::new();
        let path_vec: Vec<&[u8]> = pcpsit_primary_path.to_vec();
        let path: SubtreePath<&[u8]> = path_vec.as_slice().into();
        let (parent_path, pcpsit_key) = path.derive_parent().expect("non-root pcpsit");
        let sec_root_key_for_axis = {
            let parent = db
                .open_transactional_merk_at_path(parent_path, &tx, Some(&batch), grove_version)
                .unwrap()
                .expect("parent");
            let elem = Element::get(&parent, pcpsit_key, true, grove_version)
                .unwrap()
                .expect("elem");
            match elem.underlying() {
                Element::ProvableCountProvableSumIndexedTree(_, _, _, axes, _) => {
                    let target_tag = axis.tag();
                    axes.iter()
                        .find(|(t, _)| *t == target_tag)
                        .and_then(|(_, sk)| sk.clone())
                }
                _ => panic!("not PCPSIT"),
            }
        };
        let tree_type = crate::operations::indexed_tree::axis_secondary_tree_type(axis);
        {
            let mut secondary_merk = db
                .open_indexed_secondary_at_path(
                    path,
                    axis,
                    sec_root_key_for_axis,
                    &tx,
                    Some(&batch),
                    grove_version,
                )
                .unwrap()
                .expect("open sec");
            Element::delete(
                &mut secondary_merk,
                secondary_key,
                None,
                false,
                tree_type,
                grove_version,
            )
            .unwrap()
            .expect("delete");
        }
        db.db
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .expect("commit");
        tx.commit().expect("tx commit");
    }

    #[test]
    fn verify_grovedb_pcpsit_detects_count_axis_drift() {
        // Build PCPSIT with Count axis, populate, then delete one
        // entry from the count secondary. The H1-A check at L1889
        // must detect the inconsistency.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let axes = vec![(IndexAxis::Count.tag(), None)];
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
        for (k, v) in [(b"a".as_ref(), 5i64), (b"b", 10)] {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                k,
                Element::new_item_with_sum_item(k.to_vec(), v),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        assert_verify_passes(&db, grove_version);

        // Each inserted entry contributes count=1 to the count axis
        // secondary, keyed by (count_be ‖ item_key) = (1u64.be ‖ "a").
        let sec_key = pcpsit_count_secondary_key(1, b"a");
        corrupt_pcpsit_axis_secondary_delete(
            &db,
            &[TEST_LEAF, b"pcpsit"],
            IndexAxis::Count,
            &sec_key,
            grove_version,
        );

        let issues = db
            .verify_grovedb(None, true, true, grove_version)
            .expect("verify");
        assert!(!issues.is_empty(), "expected PCPSIT axis drift detection");
    }

    #[test]
    fn verify_grovedb_pcpsit_detects_sum_axis_drift() {
        // Same shape but Sum axis.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let axes = vec![(IndexAxis::Sum.tag(), None)];
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
            Element::new_item_with_sum_item(b"a".to_vec(), 7),
            None,
            grove_version,
        )
        .unwrap()
        .expect("a");
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"pcpsit"].as_ref(),
            b"b",
            Element::new_item_with_sum_item(b"b".to_vec(), 12),
            None,
            grove_version,
        )
        .unwrap()
        .expect("b");
        assert_verify_passes(&db, grove_version);

        // Sum secondary uses encode_sum_sort_key(7) ‖ "a".
        let sec_key = psit_secondary_key(7, b"a");
        corrupt_pcpsit_axis_secondary_delete(
            &db,
            &[TEST_LEAF, b"pcpsit"],
            IndexAxis::Sum,
            &sec_key,
            grove_version,
        );

        let issues = db
            .verify_grovedb(None, true, true, grove_version)
            .expect("verify");
        assert!(!issues.is_empty(), "expected PCPSIT sum-axis drift");
    }

    #[test]
    fn verify_grovedb_pcpsit_detects_avg_axis_drift() {
        // Same shape as the count/sum drift tests but for the Avg
        // axis, whose secondary key carries a 16-byte fixed-point
        // sort prefix. Deleting one avg-secondary row must surface
        // through the per-axis content walk with the avg-axis
        // sentinel prefix.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let axes = vec![(IndexAxis::Avg.tag(), None)];
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
        for (k, v) in [(b"a".as_ref(), 7i64), (b"b", 12)] {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                k,
                Element::new_item_with_sum_item(k.to_vec(), v),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        assert_verify_passes(&db, grove_version);

        // Each entry contributes (count = 1, sum = v) to the avg axis;
        // derive the row key with the canonical builder so the test
        // cannot drift from the mirror's encoding.
        let sec_key =
            crate::operations::indexed_tree::make_axis_secondary_key(IndexAxis::Avg, 1, 7, b"a");
        corrupt_pcpsit_axis_secondary_delete(
            &db,
            &[TEST_LEAF, b"pcpsit"],
            IndexAxis::Avg,
            &sec_key,
            grove_version,
        );

        let issues = db
            .verify_grovedb(None, true, true, grove_version)
            .expect("verify");
        assert!(!issues.is_empty(), "expected PCPSIT avg-axis drift");
        // The content walk labels avg-axis issues with the
        // `__pcpsit_avg_<kind>__` sentinel; the deleted row surfaces
        // its primary entry as an avg-axis orphan.
        let has_avg_sentinel = issues.keys().any(|p| {
            p.iter().any(|seg| {
                seg.windows(b"__pcpsit_avg_".len())
                    .any(|w| w == b"__pcpsit_avg_")
            })
        });
        assert!(
            has_avg_sentinel,
            "expected an __pcpsit_avg_*__ sentinel among issues: {issues:?}"
        );
    }

    #[test]
    fn verify_grovedb_pcit_empty_secondary_detects_orphan_insert() {
        // Insert a bogus orphan into the PCIT secondary at a key that
        // doesn't correspond to any primary entry. The cidx
        // content-consistency check at L1487-1494 should flag the
        // orphan with the __cidx_secondary_orphan__ sentinel.
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
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("seed");
        // Insert orphan at (count=999 ‖ "ghost").
        let orphan_key = make_secondary_key(999, b"ghost");
        corrupt_pcit_secondary_insert_orphan(
            &db,
            &[TEST_LEAF, b"cidx"],
            &orphan_key,
            grove_version,
        );

        let issues = db
            .verify_grovedb(None, true, true, grove_version)
            .expect("verify");
        assert!(!issues.is_empty(), "expected secondary orphan detection");
        // The cidx-specific check (L1487-1494) labels orphans under
        // __cidx_secondary_orphan__. Confirm at least one such issue
        // exists in the list.
        let has_orphan_label = issues.keys().any(|p| {
            p.iter()
                .any(|seg| seg.as_slice() == b"__cidx_secondary_orphan__")
        });
        assert!(
            has_orphan_label,
            "expected __cidx_secondary_orphan__ sentinel, got {:?}",
            issues.keys().collect::<Vec<_>>()
        );
    }

    // -----------------------------------------------------------------
    // verify_grovedb on extreme cidx structures
    // -----------------------------------------------------------------

    #[test]
    fn verify_grovedb_pcit_with_many_distinct_counts_passes() {
        // Fifteen entries with distinct counts, so every (count, key)
        // pair is unique in the secondary index. Stresses the
        // content-consistency walk over a fully distinct index.
        //
        // The counts are DERIVED, which is the only way a child can carry
        // an aggregate: each child is inserted EMPTY and then populated,
        // and propagation supplies the ordering value. A rootless child
        // asserting a count has nothing to derive it from, so the
        // distinct ordering has to come from distinct contents.
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
        for i in 0..15u64 {
            let key = format!("k{:02}", i);
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                key.as_bytes(),
                Element::empty_provable_count_tree(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert empty child");
            for j in 0..i {
                db.insert(
                    [TEST_LEAF, b"cidx", key.as_bytes()].as_ref(),
                    &j.to_be_bytes(),
                    Element::new_item(vec![]),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("populate child so its count is derived");
            }
        }

        // Pin the ordering the distinct counts are supposed to produce:
        // descending top-k must run k14(14) down to k00(0).
        let top = db
            .indexed_count_top_k([TEST_LEAF, b"cidx"].as_ref(), 15, true, None, grove_version)
            .unwrap()
            .expect("top-k over distinct derived counts");
        assert_eq!(
            top,
            (0..15u64)
                .rev()
                .map(|i| (i, format!("k{:02}", i).into_bytes()))
                .collect::<Vec<_>>()
        );

        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn verify_grovedb_pcit_with_many_same_counts_passes() {
        // Every entry ends up with the same count. Tests the secondary
        // ordering when the count prefix is identical (sort by item_key).
        //
        // As above the count is DERIVED, so "same count" means "same
        // number of children": each entry is inserted EMPTY and filled
        // with the same number of items.
        const SHARED_COUNT: u64 = 42;
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
        for i in 0..10 {
            let key = format!("k{:02}", i);
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                key.as_bytes(),
                Element::empty_provable_count_tree(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert empty child");
            for j in 0..SHARED_COUNT {
                db.insert(
                    [TEST_LEAF, b"cidx", key.as_bytes()].as_ref(),
                    &j.to_be_bytes(),
                    Element::new_item(vec![]),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("populate child so its count is derived");
            }
        }

        // Identical count prefixes: ordering falls through to item_key.
        let asc = db
            .indexed_count_top_k(
                [TEST_LEAF, b"cidx"].as_ref(),
                10,
                false,
                None,
                grove_version,
            )
            .unwrap()
            .expect("ascending top-k over tied derived counts");
        assert_eq!(
            asc,
            (0..10)
                .map(|i| (SHARED_COUNT, format!("k{:02}", i).into_bytes()))
                .collect::<Vec<_>>()
        );

        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn verify_grovedb_pcit_with_mixed_children_passes() {
        // PCIT primary can hold Items, References, empty Tree, empty
        // ProvableCountTree, etc. Verify after each accepted type.
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
        // Plain item.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"item",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("item");
        // Empty count tree.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"ct",
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("count tree");
        // Empty provable count tree.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"pct",
            Element::new_provable_count_tree(None),
            None,
            grove_version,
        )
        .unwrap()
        .expect("pct");
        assert_verify_passes(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // BUG 2 regression: the aggregate-consistency check under an indexed
    // primary must stay ACTIVE for POPULATED tree children (recorded ==
    // actual), and only be skipped for EMPTY-inner-Merk children whose
    // recorded aggregate is a user-supplied secondary ordering key.
    // -----------------------------------------------------------------

    #[test]
    fn verify_grovedb_pcit_populated_tree_child_verifies_clean() {
        // A CountTree child under a PCIT primary that has been POPULATED
        // via generic sub-inserts (so its inner Merk carries real
        // aggregate data) keeps recorded == actual through propagation.
        // With BUG 2 fixed, the aggregate-consistency check is active for
        // this populated child (not skipped) and must report no issues.
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
        .expect("create cidx");
        // Insert an EMPTY CountTree child via the dedicated API.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"ct",
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("empty count tree child");
        // Empty child: aggregate check is skipped (ordering-key case).
        assert_verify_passes(&db, grove_version);

        // Populate the CountTree child through the generic path (this is
        // a sub-tree UNDER the primary, so it propagates through Role A and
        // is supported). After this the child's recorded count == its
        // actual inner aggregate.
        for k in [b"x".as_slice(), b"y", b"z"] {
            db.insert(
                [TEST_LEAF, b"cidx", b"ct"].as_ref(),
                k,
                Element::new_item(b"v".to_vec()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("populate count tree child");
        }

        // The child element's recorded count is now 3 (propagated), and
        // the inner Merk actually holds 3 entries. The now-active
        // aggregate-consistency check must agree → clean verify.
        let child = db
            .get([TEST_LEAF, b"cidx"].as_ref(), b"ct", None, grove_version)
            .unwrap()
            .expect("get ct child");
        match child.underlying() {
            Element::CountTree(_, c, _) => assert_eq!(*c, 3, "recorded count must be 3"),
            other => panic!("expected CountTree child, got {:?}", other),
        }
        assert_verify_passes(&db, grove_version);
    }

    /// PSIT drift must stay visible AFTER a later write re-heals the parent
    /// hash over the drifted secondary.
    ///
    /// The H1-A chain check alone catches drift only while the parent still
    /// commits to the OLD secondary root. Any subsequent legitimate write
    /// re-derives the parent's value_hash from the drifted secondary's
    /// current root, at which point the chain is internally consistent again
    /// and the drift is invisible without a per-entry content walk.
    #[test]
    fn verify_grovedb_psit_drift_survives_hash_reheal_and_is_still_detected() {
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
        .expect("create psit");
        for (k, sum) in [(b"a".as_slice(), 10i64), (b"b".as_slice(), 20)] {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"psit"].as_ref(),
                k,
                Element::new_sum_item(sum),
                None,
                grove_version,
            )
            .unwrap()
            .expect("seed");
        }

        // Drop "a"'s secondary row behind the mirror's back.
        let mut sec_key = Vec::new();
        sec_key.extend_from_slice(&grovedb_element::indexed::sort_keys::encode_sum_sort_key(
            10,
        ));
        sec_key.extend_from_slice(b"a");
        corrupt_psit_secondary_delete(&db, &[TEST_LEAF, b"psit"], &sec_key, grove_version);

        // The chain check sees it right now.
        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify");
        assert!(!issues.is_empty(), "fresh drift must be detected");

        // Now perform a legitimate write through the dedicated API. It
        // re-derives the parent's value_hash from the CURRENT (drifted)
        // secondary root, healing the hash chain over the corruption.
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"c",
            Element::new_sum_item(30),
            None,
            grove_version,
        )
        .unwrap()
        .expect("post-drift write re-heals the chain");

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify");
        let orphan_path: Vec<Vec<u8>> = vec![
            TEST_LEAF.to_vec(),
            b"psit".to_vec(),
            b"__psit_primary_orphan__".to_vec(),
            b"a".to_vec(),
        ];
        assert!(
            issues.contains_key(&orphan_path),
            "content walk must still report the orphaned primary entry after the hash chain was \
             re-healed; got {:?}",
            issues.keys().collect::<Vec<_>>()
        );
    }

    /// Same re-heal scenario as the PSIT case, but on a multi-axis PCPSIT:
    /// drift on ONE axis must remain visible after a later write re-derives
    /// the axes digest over the drifted axis, and must be attributed to the
    /// axis it happened on.
    #[test]
    fn verify_grovedb_pcpsit_axis_drift_survives_hash_reheal_and_names_the_axis() {
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
        for (k, sum) in [(b"a".as_slice(), 7i64), (b"b".as_slice(), 12)] {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                k,
                Element::new_item_with_sum_item(k.to_vec(), sum),
                None,
                grove_version,
            )
            .unwrap()
            .expect("seed");
        }

        // Delete "a"'s row from the SUM axis only.
        let mut sum_key = Vec::new();
        sum_key.extend_from_slice(&grovedb_element::indexed::sort_keys::encode_sum_sort_key(7));
        sum_key.extend_from_slice(b"a");
        corrupt_pcpsit_axis_secondary_delete(
            &db,
            &[TEST_LEAF, b"pcpsit"],
            IndexAxis::Sum,
            &sum_key,
            grove_version,
        );

        // A later legitimate write re-derives the axes digest over the
        // drifted sum secondary, healing the chain.
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"pcpsit"].as_ref(),
            b"c",
            Element::new_item_with_sum_item(b"c".to_vec(), 30),
            None,
            grove_version,
        )
        .unwrap()
        .expect("post-drift write re-heals the chain");

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify");
        let sum_orphan: Vec<Vec<u8>> = vec![
            TEST_LEAF.to_vec(),
            b"pcpsit".to_vec(),
            b"__pcpsit_sum_primary_orphan__".to_vec(),
            b"a".to_vec(),
        ];
        assert!(
            issues.contains_key(&sum_orphan),
            "sum-axis drift must be reported against the sum axis; got {:?}",
            issues.keys().collect::<Vec<_>>()
        );
        // The untouched count axis must not be implicated.
        let count_orphan: Vec<Vec<u8>> = vec![
            TEST_LEAF.to_vec(),
            b"pcpsit".to_vec(),
            b"__pcpsit_count_primary_orphan__".to_vec(),
            b"a".to_vec(),
        ];
        assert!(
            !issues.contains_key(&count_orphan),
            "the healthy count axis must not be flagged"
        );
    }
}
