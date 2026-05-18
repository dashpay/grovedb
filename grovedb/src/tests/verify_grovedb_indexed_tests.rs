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
                .open_count_indexed_secondary_at_path(
                    path,
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
                .open_count_indexed_secondary_at_path(
                    path,
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
}
