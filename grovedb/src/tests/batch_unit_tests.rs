//! Unit tests for batch module types and helpers.
//!
//! These tests cover `NonMerkTreeMeta`, `GroveOp` ordering, `KeyInfoPath`
//! utilities, `QualifiedGroveDbOp` constructors/Debug,
//! `verify_consistency_of_operations`, `apply_operations_without_batching`,
//! non-Merk tree propagation in batch, and reference chain resolution in batch.

#[cfg(feature = "minimal")]
mod tests {
    use grovedb_merk::tree::AggregateData;
    use grovedb_merk::tree_type::TreeType;
    use grovedb_version::version::GroveVersion;

    use crate::batch::key_info::KeyInfo::KnownKey;
    use crate::batch::{GroveOp, KeyInfoPath, NonMerkTreeMeta, QualifiedGroveDbOp};
    use crate::reference_path::ReferencePathType;
    use crate::tests::{common::EMPTY_PATH, make_empty_grovedb, make_test_grovedb, TEST_LEAF};
    use crate::Element;

    // ===================================================================
    // Group 1: NonMerkTreeMeta::to_tree_type() and count()
    // ===================================================================

    #[test]
    fn test_non_merk_tree_meta_to_tree_type() {
        let ct = NonMerkTreeMeta::CommitmentTree {
            total_count: 5,
            chunk_power: 3,
        };
        assert_eq!(ct.to_tree_type(), TreeType::CommitmentTree(3));

        let mmr = NonMerkTreeMeta::MmrTree { mmr_size: 99 };
        assert_eq!(mmr.to_tree_type(), TreeType::MmrTree);

        let bulk = NonMerkTreeMeta::BulkAppendTree {
            total_count: 50,
            chunk_power: 7,
        };
        assert_eq!(bulk.to_tree_type(), TreeType::BulkAppendTree(7));

        let dt = NonMerkTreeMeta::DenseTree {
            count: 10,
            height: 5,
        };
        assert_eq!(dt.to_tree_type(), TreeType::DenseAppendOnlyFixedSizeTree(5));
    }

    #[test]
    fn test_non_merk_tree_meta_count() {
        let ct = NonMerkTreeMeta::CommitmentTree {
            total_count: 42,
            chunk_power: 3,
        };
        assert_eq!(ct.count(), 42);

        let mmr = NonMerkTreeMeta::MmrTree { mmr_size: 99 };
        assert_eq!(mmr.count(), 99);

        let bulk = NonMerkTreeMeta::BulkAppendTree {
            total_count: 200,
            chunk_power: 5,
        };
        assert_eq!(bulk.count(), 200);

        let dt = NonMerkTreeMeta::DenseTree {
            count: 7,
            height: 4,
        };
        assert_eq!(dt.count(), 7);
    }

    // ===================================================================
    // Group 2: GroveOp ordering — all 16 variants
    // ===================================================================

    #[test]
    fn test_grove_op_ord_all_variants() {
        let dummy_element = Element::new_item(b"x".to_vec());
        let dummy_hash = [0u8; 32];
        let meta_commitment = NonMerkTreeMeta::CommitmentTree {
            total_count: 0,
            chunk_power: 2,
        };

        let all_ops: Vec<GroveOp> = vec![
            GroveOp::DeleteTree(TreeType::NormalTree), // 0
            GroveOp::Delete,                           // 2
            GroveOp::InsertTreeWithRootHash {
                // 3
                hash: dummy_hash,
                root_key: None,
                flags: None,
                aggregate_data: AggregateData::NoAggregateData,
            },
            GroveOp::ReplaceTreeRootKey {
                // 4
                hash: dummy_hash,
                root_key: None,
                aggregate_data: AggregateData::NoAggregateData,
            },
            GroveOp::RefreshReference {
                // 5
                reference_path_type: ReferencePathType::AbsolutePathReference(vec![]),
                max_reference_hop: None,
                flags: None,
                trust_refresh_reference: false,
            },
            GroveOp::Replace {
                // 6
                element: dummy_element.clone(),
            },
            GroveOp::Patch {
                // 7
                element: dummy_element.clone(),
                change_in_bytes: 0,
            },
            GroveOp::InsertOrReplace {
                // 8
                element: dummy_element.clone(),
            },
            GroveOp::InsertOnly {
                // 9
                element: dummy_element.clone(),
            },
            GroveOp::CommitmentTreeInsert {
                // 10
                cmx: dummy_hash,
                rho: dummy_hash,
                payload: vec![],
            },
            GroveOp::MmrTreeAppend { value: vec![] },   // 11
            GroveOp::BulkAppend { value: vec![] },      // 12
            GroveOp::DenseTreeInsert { value: vec![] }, // 13
            GroveOp::ReplaceNonMerkTreeRoot {
                // 14
                hash: dummy_hash,
                meta: meta_commitment.clone(),
            },
            GroveOp::InsertNonMerkTree {
                // 15
                hash: dummy_hash,
                root_key: None,
                flags: None,
                aggregate_data: AggregateData::NoAggregateData,
                meta: meta_commitment,
            },
        ];

        // Verify they are already in sorted order
        for window in all_ops.windows(2) {
            assert!(
                window[0] < window[1],
                "{:?} should be less than {:?}",
                window[0],
                window[1]
            );
        }

        // Also verify PartialOrd consistency
        for (i, a) in all_ops.iter().enumerate() {
            for (j, b) in all_ops.iter().enumerate() {
                let expected = i.cmp(&j);
                assert_eq!(
                    a.partial_cmp(b),
                    Some(expected),
                    "partial_cmp mismatch at indices ({}, {})",
                    i,
                    j
                );
            }
        }
    }

    // ===================================================================
    // Group 3: KeyInfoPath utilities
    // ===================================================================

    #[test]
    fn test_key_info_path_eq_vec_types() {
        let path = KeyInfoPath::from_known_path([b"x".as_ref(), b"y".as_ref()]);

        // PartialEq<Vec<Vec<u8>>> — matching
        let vv: Vec<Vec<u8>> = vec![b"x".to_vec(), b"y".to_vec()];
        assert_eq!(path, vv);

        // Length mismatch
        let short: Vec<Vec<u8>> = vec![b"x".to_vec()];
        assert_ne!(path, short);

        // Content mismatch
        let wrong: Vec<Vec<u8>> = vec![b"x".to_vec(), b"z".to_vec()];
        assert_ne!(path, wrong);

        // PartialEq<Vec<&[u8]>> — matching
        let vr: Vec<&[u8]> = vec![b"x", b"y"];
        assert_eq!(path, vr);

        // Length mismatch
        let short_r: Vec<&[u8]> = vec![b"x"];
        assert_ne!(path, short_r);

        // PartialEq<[&[u8]; N]> — matching
        assert_eq!(path, [b"x".as_ref(), b"y".as_ref()]);

        // Length mismatch (array of 1)
        let one_path = KeyInfoPath::from_known_path([b"x".as_ref()]);
        assert_ne!(one_path, [b"x".as_ref(), b"y".as_ref()]);

        // Length mismatch (array of 3)
        assert_ne!(path, [b"x".as_ref(), b"y".as_ref(), b"z".as_ref()]);
    }

    #[test]
    fn test_key_info_path_methods() {
        // from_vec
        let kip = KeyInfoPath::from_vec(vec![KnownKey(b"a".to_vec()), KnownKey(b"b".to_vec())]);
        assert_eq!(kip.len(), 2);
        assert!(!kip.is_empty());

        // from_known_path
        let kip2 = KeyInfoPath::from_known_path([b"a".as_ref(), b"b".as_ref()]);
        assert_eq!(kip, kip2);

        // from_known_owned_path
        let kip3 = KeyInfoPath::from_known_owned_path(vec![b"a".to_vec(), b"b".to_vec()]);
        assert_eq!(kip, kip3);

        // to_path
        assert_eq!(kip.to_path(), vec![b"a".to_vec(), b"b".to_vec()]);

        // to_path_consume
        let consumed = kip.clone().to_path_consume();
        assert_eq!(consumed, vec![b"a".to_vec(), b"b".to_vec()]);

        // to_path_refs
        let refs = kip.to_path_refs();
        assert_eq!(refs, vec![b"a".as_ref(), b"b".as_ref()]);

        // split_last
        let (last, rest) = kip.split_last().unwrap();
        assert_eq!(*last, KnownKey(b"b".to_vec()));
        assert_eq!(rest.len(), 1);

        // last
        assert_eq!(*kip.last().unwrap(), KnownKey(b"b".to_vec()));

        // as_vec
        assert_eq!(kip.as_vec().len(), 2);

        // push
        let mut kip_mut = kip.clone();
        kip_mut.push(KnownKey(b"c".to_vec()));
        assert_eq!(kip_mut.len(), 3);

        // iterator
        let keys: Vec<_> = kip.iterator().collect();
        assert_eq!(keys.len(), 2);

        // into_iterator
        let keys: Vec<_> = kip.into_iterator().collect();
        assert_eq!(keys.len(), 2);

        // empty path
        let empty = KeyInfoPath::default();
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);
        assert!(empty.split_last().is_none());
        assert!(empty.last().is_none());
    }

    // ===================================================================
    // Group 4: QualifiedGroveDbOp constructors & Debug
    // ===================================================================

    #[test]
    fn test_qualified_op_estimated_constructors() {
        let path = KeyInfoPath::from_known_path([b"a".as_ref()]);
        let key = KnownKey(b"k".to_vec());
        let element = Element::new_item(b"v".to_vec());

        // insert_estimated_op
        let op =
            QualifiedGroveDbOp::insert_estimated_op(path.clone(), key.clone(), element.clone());
        assert!(matches!(op.op, GroveOp::InsertOrReplace { .. }));
        assert_eq!(op.path, path);
        assert_eq!(op.key, Some(key.clone()));

        // replace_estimated_op
        let op =
            QualifiedGroveDbOp::replace_estimated_op(path.clone(), key.clone(), element.clone());
        assert!(matches!(op.op, GroveOp::Replace { .. }));

        // patch_estimated_op
        let op =
            QualifiedGroveDbOp::patch_estimated_op(path.clone(), key.clone(), element.clone(), 5);
        assert!(matches!(
            op.op,
            GroveOp::Patch {
                change_in_bytes: 5,
                ..
            }
        ));

        // delete_estimated_op
        let op = QualifiedGroveDbOp::delete_estimated_op(path.clone(), key.clone());
        assert!(matches!(op.op, GroveOp::Delete));

        // delete_estimated_tree_op
        let op = QualifiedGroveDbOp::delete_estimated_tree_op(
            path.clone(),
            key.clone(),
            TreeType::NormalTree,
        );
        assert!(matches!(op.op, GroveOp::DeleteTree(TreeType::NormalTree)));
    }

    #[test]
    fn test_qualified_op_debug_all_variants() {
        let element = Element::new_item(b"val".to_vec());
        let hash = [0u8; 32];

        // Keyed ops
        let ops_with_expected: Vec<(QualifiedGroveDbOp, &str)> = vec![
            (
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![b"p".to_vec()],
                    b"k".to_vec(),
                    element.clone(),
                ),
                "Insert Or Replace",
            ),
            (
                QualifiedGroveDbOp::insert_only_op(
                    vec![b"p".to_vec()],
                    b"k".to_vec(),
                    element.clone(),
                ),
                "Insert",
            ),
            (
                QualifiedGroveDbOp::replace_op(vec![b"p".to_vec()], b"k".to_vec(), element.clone()),
                "Replace",
            ),
            (
                QualifiedGroveDbOp::patch_op(
                    vec![b"p".to_vec()],
                    b"k".to_vec(),
                    element.clone(),
                    3,
                ),
                "Patch",
            ),
            (
                QualifiedGroveDbOp::delete_op(vec![b"p".to_vec()], b"k".to_vec()),
                "Delete",
            ),
            (
                QualifiedGroveDbOp::delete_tree_op(
                    vec![b"p".to_vec()],
                    b"k".to_vec(),
                    TreeType::NormalTree,
                ),
                "Delete Tree",
            ),
            (
                QualifiedGroveDbOp::refresh_reference_op(
                    vec![b"p".to_vec()],
                    b"k".to_vec(),
                    ReferencePathType::AbsolutePathReference(vec![]),
                    None,
                    None,
                    false,
                ),
                "Refresh Reference",
            ),
        ];

        for (op, expected_substr) in &ops_with_expected {
            let dbg = format!("{:?}", op);
            assert!(
                dbg.contains(expected_substr),
                "Debug for {:?} should contain '{}', got: {}",
                op.op,
                expected_substr,
                dbg
            );
        }

        // Keyless ops
        let keyless_ops: Vec<(QualifiedGroveDbOp, &str)> = vec![
            (
                QualifiedGroveDbOp::commitment_tree_insert_op(
                    vec![b"pool".to_vec()],
                    hash,
                    hash,
                    vec![],
                ),
                "Commitment Tree Insert",
            ),
            (
                QualifiedGroveDbOp::mmr_tree_append_op(vec![b"mmr".to_vec()], vec![]),
                "MMR Tree Append",
            ),
            (
                QualifiedGroveDbOp::bulk_append_op(vec![b"bulk".to_vec()], vec![]),
                "Bulk Append",
            ),
            (
                QualifiedGroveDbOp::dense_tree_insert_op(vec![b"dense".to_vec()], vec![]),
                "Dense Tree Insert",
            ),
        ];

        for (op, expected_substr) in &keyless_ops {
            let dbg = format!("{:?}", op);
            assert!(
                dbg.contains(expected_substr),
                "Debug should contain '{}', got: {}",
                expected_substr,
                dbg
            );
            assert!(
                dbg.contains("(keyless)"),
                "Keyless op debug should contain '(keyless)', got: {}",
                dbg
            );
        }

        // Internal ops (ReplaceTreeRootKey, InsertTreeWithRootHash, etc.)
        let internal_op = QualifiedGroveDbOp {
            path: KeyInfoPath::from_known_path([b"p".as_ref()]),
            key: Some(KnownKey(b"k".to_vec())),
            op: GroveOp::ReplaceTreeRootKey {
                hash,
                root_key: None,
                aggregate_data: AggregateData::NoAggregateData,
            },
        };
        let dbg = format!("{:?}", internal_op);
        assert!(dbg.contains("Replace Tree Hash and Root Key"));

        let internal_op2 = QualifiedGroveDbOp {
            path: KeyInfoPath::from_known_path([b"p".as_ref()]),
            key: Some(KnownKey(b"k".to_vec())),
            op: GroveOp::InsertTreeWithRootHash {
                hash,
                root_key: None,
                flags: None,
                aggregate_data: AggregateData::NoAggregateData,
            },
        };
        let dbg = format!("{:?}", internal_op2);
        assert!(dbg.contains("Insert Tree Hash and Root Key"));

        // ReplaceNonMerkTreeRoot
        let meta = NonMerkTreeMeta::MmrTree { mmr_size: 5 };
        let internal_op3 = QualifiedGroveDbOp {
            path: KeyInfoPath::from_known_path([b"p".as_ref()]),
            key: Some(KnownKey(b"k".to_vec())),
            op: GroveOp::ReplaceNonMerkTreeRoot { hash, meta },
        };
        let dbg = format!("{:?}", internal_op3);
        assert!(dbg.contains("Replace Non-Merk Tree Root"));

        // InsertNonMerkTree
        let meta2 = NonMerkTreeMeta::DenseTree {
            count: 1,
            height: 3,
        };
        let internal_op4 = QualifiedGroveDbOp {
            path: KeyInfoPath::from_known_path([b"p".as_ref()]),
            key: Some(KnownKey(b"k".to_vec())),
            op: GroveOp::InsertNonMerkTree {
                hash,
                root_key: None,
                flags: None,
                aggregate_data: AggregateData::NoAggregateData,
                meta: meta2,
            },
        };
        let dbg = format!("{:?}", internal_op4);
        assert!(dbg.contains("Insert Non-Merk Tree"));
    }

    // ===================================================================
    // Group 5: verify_consistency_of_operations()
    // ===================================================================

    #[test]
    fn test_consistency_duplicate_ops() {
        let op = QualifiedGroveDbOp::insert_or_replace_op(
            vec![b"root".to_vec()],
            b"key1".to_vec(),
            Element::new_item(b"a".to_vec()),
        );
        // Three copies of the same op
        let ops = vec![op.clone(), op.clone(), op];
        let results = QualifiedGroveDbOp::verify_consistency_of_operations(&ops);
        assert!(
            !results.is_empty(),
            "duplicate ops should be flagged as inconsistent"
        );
    }

    #[test]
    fn test_consistency_append_delete_conflict() {
        let ops = vec![
            QualifiedGroveDbOp::mmr_tree_append_op(
                vec![b"root".to_vec(), b"tree_key".to_vec()],
                b"data".to_vec(),
            ),
            QualifiedGroveDbOp::delete_op(vec![b"root".to_vec()], b"tree_key".to_vec()),
        ];
        let results = QualifiedGroveDbOp::verify_consistency_of_operations(&ops);
        assert!(
            !results.is_empty(),
            "append + delete targeting same tree should be flagged"
        );
    }

    #[test]
    fn test_consistency_insert_only_under_deleted_path() {
        let ops = vec![
            QualifiedGroveDbOp::delete_op(vec![b"root".to_vec()], b"subtree".to_vec()),
            QualifiedGroveDbOp::insert_only_op(
                vec![b"root".to_vec(), b"subtree".to_vec()],
                b"key".to_vec(),
                Element::new_item(b"val".to_vec()),
            ),
        ];
        let results = QualifiedGroveDbOp::verify_consistency_of_operations(&ops);
        assert!(
            !results.is_empty(),
            "InsertOnly under a deleted path should be flagged"
        );
    }

    #[test]
    fn test_consistency_insert_under_delete_tree_path() {
        let ops = vec![
            QualifiedGroveDbOp::delete_tree_op(
                vec![b"root".to_vec()],
                b"subtree".to_vec(),
                TreeType::NormalTree,
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"root".to_vec(), b"subtree".to_vec()],
                b"key".to_vec(),
                Element::new_item(b"val".to_vec()),
            ),
        ];
        let results = QualifiedGroveDbOp::verify_consistency_of_operations(&ops);
        assert!(
            !results.is_empty(),
            "insert under a DeleteTree path should be flagged"
        );
    }

    // ===================================================================
    // Group 6: apply_operations_without_batching — non-Merk tree ops
    // ===================================================================

    #[test]
    fn test_apply_without_batching_commitment_tree_insert() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"ct",
            Element::empty_commitment_tree(10).expect("valid chunk_power"),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert commitment tree");

        let cmx = {
            let mut bytes = [0u8; 32];
            bytes[0] = 1;
            bytes[31] &= 0x7f;
            bytes
        };
        let rho = [2u8; 32];

        let ops = vec![QualifiedGroveDbOp::commitment_tree_insert_op(
            vec![b"ct".to_vec()],
            cmx,
            rho,
            vec![0u8; 216],
        )];

        db.apply_operations_without_batching(ops, None, None, grove_version)
            .unwrap()
            .expect("commitment tree insert via apply_operations_without_batching");
    }

    #[test]
    fn test_apply_without_batching_mmr_tree_append() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"mmr",
            Element::empty_mmr_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert mmr tree");

        let ops = vec![QualifiedGroveDbOp::mmr_tree_append_op(
            vec![b"mmr".to_vec()],
            b"leaf_data".to_vec(),
        )];

        db.apply_operations_without_batching(ops, None, None, grove_version)
            .unwrap()
            .expect("mmr tree append via apply_operations_without_batching");
    }

    #[test]
    fn test_apply_without_batching_bulk_append() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"bulk",
            Element::empty_bulk_append_tree(10).expect("valid chunk_power"),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert bulk append tree");

        let ops = vec![QualifiedGroveDbOp::bulk_append_op(
            vec![b"bulk".to_vec()],
            b"payload".to_vec(),
        )];

        db.apply_operations_without_batching(ops, None, None, grove_version)
            .unwrap()
            .expect("bulk append via apply_operations_without_batching");
    }

    #[test]
    fn test_apply_without_batching_dense_tree_insert() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"dense",
            Element::empty_dense_tree(4),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert dense tree");

        let ops = vec![QualifiedGroveDbOp::dense_tree_insert_op(
            vec![b"dense".to_vec()],
            b"dense_data".to_vec(),
        )];

        db.apply_operations_without_batching(ops, None, None, grove_version)
            .unwrap()
            .expect("dense tree insert via apply_operations_without_batching");
    }

    // ===================================================================
    // Group 7: Non-Merk tree propagation in apply_batch
    //
    // Exercises the propagation branches at L2400-2469 where a non-Merk
    // tree element in an Occupied entry is converted to InsertNonMerkTree.
    // Pattern: batch-insert the non-Merk tree AND an item under it so
    // the root hash propagates to the parent's occupied InsertOrReplace.
    // ===================================================================

    #[test]
    fn test_batch_propagation_commitment_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Insert a parent tree so the CommitmentTree is at level 1
        db.insert(
            EMPTY_PATH,
            b"parent",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert parent");

        // Batch: create CommitmentTree under parent AND insert an item under it.
        // The item triggers propagation back to the CommitmentTree entry.
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"parent".to_vec()],
                b"ct".to_vec(),
                Element::empty_commitment_tree(10).expect("valid chunk_power"),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"parent".to_vec(), b"ct".to_vec()],
                b"item".to_vec(),
                Element::new_item(b"value".to_vec()),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch propagation through CommitmentTree");
    }

    #[test]
    fn test_batch_propagation_mmr_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"parent",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert parent");

        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"parent".to_vec()],
                b"mmr".to_vec(),
                Element::empty_mmr_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"parent".to_vec(), b"mmr".to_vec()],
                b"item".to_vec(),
                Element::new_item(b"value".to_vec()),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch propagation through MmrTree");
    }

    #[test]
    fn test_batch_propagation_bulk_append_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"parent",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert parent");

        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"parent".to_vec()],
                b"bulk".to_vec(),
                Element::empty_bulk_append_tree(10).expect("valid chunk_power"),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"parent".to_vec(), b"bulk".to_vec()],
                b"item".to_vec(),
                Element::new_item(b"value".to_vec()),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch propagation through BulkAppendTree");
    }

    #[test]
    fn test_batch_propagation_dense_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"parent",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert parent");

        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"parent".to_vec()],
                b"dense".to_vec(),
                Element::empty_dense_tree(4),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"parent".to_vec(), b"dense".to_vec()],
                b"item".to_vec(),
                Element::new_item(b"value".to_vec()),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch propagation through DenseTree");
    }

    // ===================================================================
    // Group 8: Reference chain resolution in batch
    //
    // Exercises follow_reference_get_value_hash and process_reference
    // code paths that are not reached by existing tests.
    // ===================================================================

    /// Reference to an item being InsertOnly'd in the same batch.
    /// Exercises the InsertOnly { Item } branch (L1469-1476).
    #[test]
    fn test_batch_ref_to_insert_only_item() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let ops = vec![
            QualifiedGroveDbOp::insert_only_op(
                vec![TEST_LEAF.to_vec()],
                b"only_item".to_vec(),
                Element::new_item(b"hello".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"ref_to_only".to_vec(),
                Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                    TEST_LEAF.to_vec(),
                    b"only_item".to_vec(),
                ])),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch ref to InsertOnly item");

        // Verify the reference resolves
        let result = db
            .get([TEST_LEAF].as_ref(), b"ref_to_only", None, grove_version)
            .unwrap()
            .expect("should resolve reference");
        assert_eq!(result, Element::new_item(b"hello".to_vec()));
    }

    /// Reference chain through an InsertOnly reference in the same batch.
    /// ref_a → ref_b (InsertOnly) → existing item on disk.
    /// Exercises the InsertOnly { Reference } branch (L1478-1490).
    #[test]
    fn test_batch_ref_to_insert_only_reference_chain() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Put base item on disk first
        db.insert(
            [TEST_LEAF].as_ref(),
            b"base_item",
            Element::new_item(b"base_val".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert base item");

        // Batch: InsertOnly a reference to base_item, then another ref to that
        let ops = vec![
            QualifiedGroveDbOp::insert_only_op(
                vec![TEST_LEAF.to_vec()],
                b"ref_b".to_vec(),
                Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                    TEST_LEAF.to_vec(),
                    b"base_item".to_vec(),
                ])),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"ref_a".to_vec(),
                Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                    TEST_LEAF.to_vec(),
                    b"ref_b".to_vec(),
                ])),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch ref chain through InsertOnly reference");

        let result = db
            .get([TEST_LEAF].as_ref(), b"ref_a", None, grove_version)
            .unwrap()
            .expect("should resolve ref_a");
        assert_eq!(result, Element::new_item(b"base_val".to_vec()));
    }

    /// Reference pointing to a tree element being inserted in the same batch.
    /// Should error because references cannot point to trees.
    /// Exercises the Tree error branch in follow_reference_get_value_hash
    /// (L1451-1465).
    #[test]
    fn test_batch_ref_to_tree_in_batch_errors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"new_tree".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"ref_to_tree".to_vec(),
                Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                    TEST_LEAF.to_vec(),
                    b"new_tree".to_vec(),
                ])),
            ),
        ];

        let result = db.apply_batch(ops, None, None, grove_version).unwrap();
        assert!(result.is_err(), "reference to tree in batch should fail");
    }

    /// Reference pointing to an item being deleted in the same batch.
    /// Should error because references cannot point to deleted elements.
    /// Exercises the Delete/DeleteTree error branch (L1530-1533).
    #[test]
    fn test_batch_ref_to_deleted_item_errors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert item on disk
        db.insert(
            [TEST_LEAF].as_ref(),
            b"to_delete",
            Element::new_item(b"val".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item");

        // Batch: delete that item AND insert a reference to it
        let ops = vec![
            QualifiedGroveDbOp::delete_op(vec![TEST_LEAF.to_vec()], b"to_delete".to_vec()),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"ref_to_deleted".to_vec(),
                Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                    TEST_LEAF.to_vec(),
                    b"to_delete".to_vec(),
                ])),
            ),
        ];

        let result = db.apply_batch(ops, None, None, grove_version).unwrap();
        assert!(result.is_err(), "reference to deleted element should fail");
    }

    /// Reference chain through an existing reference on disk (hop > 1).
    /// ref_a (batch) → ref_b (on disk) → base_item (on disk).
    /// Exercises process_reference_with_hop_count_greater_than_one
    /// Element::Reference branch (L1300-1312).
    #[test]
    fn test_batch_ref_chain_through_existing_reference() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Set up on disk: base_item and ref_b → base_item
        db.insert(
            [TEST_LEAF].as_ref(),
            b"base_item",
            Element::new_item(b"chain_val".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert base item");

        db.insert(
            [TEST_LEAF].as_ref(),
            b"ref_b",
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"base_item".to_vec(),
            ])),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert ref_b");

        // Batch: insert ref_a → ref_b with hops > 1
        // ref_b is NOT in the batch, so process_reference is called,
        // reads ref_b from disk, sees Element::Reference, follows chain.
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"ref_a".to_vec(),
            Element::new_reference_with_hops(
                ReferencePathType::AbsolutePathReference(vec![
                    TEST_LEAF.to_vec(),
                    b"ref_b".to_vec(),
                ]),
                Some(5),
            ),
        )];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch ref chain through existing reference");

        let result = db
            .get([TEST_LEAF].as_ref(), b"ref_a", None, grove_version)
            .unwrap()
            .expect("should resolve ref_a through chain");
        assert_eq!(result, Element::new_item(b"chain_val".to_vec()));
    }

    /// Reference to an existing tree on disk with hop > 1.
    /// Should error via process_reference_with_hop_count_greater_than_one
    /// (L1314-1327).
    #[test]
    fn test_batch_ref_to_existing_tree_errors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // The test_leaf itself is a tree on disk. Insert a ref pointing to it.
        // With hops > 1 so we go through process_reference_with_hop_count_greater_than_one.
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"ref_to_tree".to_vec(),
            Element::new_reference_with_hops(
                ReferencePathType::AbsolutePathReference(vec![
                    // Point to ANOTHER_TEST_LEAF which is a tree at root level
                    b"test_leaf2".to_vec(),
                ]),
                Some(3),
            ),
        )];

        let result = db.apply_batch(ops, None, None, grove_version).unwrap();
        assert!(result.is_err(), "reference to existing tree should fail");
    }

    /// Reference with max_hop=1 to an existing item not in the batch.
    /// Exercises the process_reference hop=1 branch (L1070-1100).
    #[test]
    fn test_batch_ref_hop_one_to_existing_item() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert target on disk
        db.insert(
            [TEST_LEAF].as_ref(),
            b"target",
            Element::new_item(b"hop1_val".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert target");

        // Batch: insert reference with max_hops = 1
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"ref_hop1".to_vec(),
            Element::new_reference_with_hops(
                ReferencePathType::AbsolutePathReference(vec![
                    TEST_LEAF.to_vec(),
                    b"target".to_vec(),
                ]),
                Some(1),
            ),
        )];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch ref with hop=1 to existing item");

        let result = db
            .get([TEST_LEAF].as_ref(), b"ref_hop1", None, grove_version)
            .unwrap()
            .expect("should resolve hop-1 ref");
        assert_eq!(result, Element::new_item(b"hop1_val".to_vec()));
    }

    /// RefreshReference with trust_refresh_reference=false reads the element
    /// from disk before processing.
    /// RefreshReference on an element that is NOT a reference on disk.
    /// Should error (L1813-1817).
    #[test]
    fn test_batch_refresh_ref_on_non_reference_errors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert a plain item (not a reference)
        db.insert(
            [TEST_LEAF].as_ref(),
            b"plain_item",
            Element::new_item(b"not_a_ref".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert plain item");

        // Batch: try to refresh it as if it were a reference (trust=false)
        let ops = vec![QualifiedGroveDbOp::refresh_reference_op(
            vec![TEST_LEAF.to_vec()],
            b"plain_item".to_vec(),
            ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"something".to_vec(),
            ]),
            None,
            None,
            false,
        )];

        let result = db.apply_batch(ops, None, None, grove_version).unwrap();
        assert!(result.is_err(), "refreshing a non-reference should fail");
    }

    /// Batch-insert a CommitmentTree element via insert_item_element.
    /// This exercises the CommitmentTree branch (L1710-1735) that computes
    /// the empty state root for a new CommitmentTree element.
    #[test]
    fn test_batch_commitment_tree_element_in_execute_ops() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Create parent tree
        db.insert(
            EMPTY_PATH,
            b"parent",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert parent");

        // Batch-insert a CommitmentTree directly (no items under it, so
        // insert_item_element handles it and computes the empty state root).
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![b"parent".to_vec()],
            b"ct_elem".to_vec(),
            Element::empty_commitment_tree(10).expect("valid chunk_power"),
        )];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch insert CommitmentTree element");

        // Verify the tree was inserted
        let elem = db
            .get([b"parent"].as_ref(), b"ct_elem", None, grove_version)
            .unwrap()
            .expect("should get CommitmentTree");
        assert!(elem.is_commitment_tree());
    }

    // ===================================================================
    // Group 11: follow_reference_get_value_hash — Replace & Patch variants
    //           (L1382-1383)
    // ===================================================================

    #[test]
    fn test_batch_ref_to_replace_op_item() {
        // A reference in the batch points to a key that is being Replace'd
        // (not InsertOrReplace) with an Item — covers GroveOp::Replace branch
        // in follow_reference_get_value_hash.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // First, insert the original item so Replace can find it
        db.insert(
            [TEST_LEAF].as_ref(),
            b"target",
            Element::new_item(b"old_value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert original item");

        // Now batch: Replace the target + insert a reference to it
        let ops = vec![
            QualifiedGroveDbOp::replace_op(
                vec![TEST_LEAF.to_vec()],
                b"target".to_vec(),
                Element::new_item(b"new_value".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"ref_to_target".to_vec(),
                Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                    TEST_LEAF.to_vec(),
                    b"target".to_vec(),
                ])),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch with Replace + ref should succeed");
    }

    #[test]
    fn test_batch_ref_to_patch_op_item() {
        // A reference in the batch points to a key that is being Patch'd
        // — covers GroveOp::Patch branch in follow_reference_get_value_hash.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert original item
        db.insert(
            [TEST_LEAF].as_ref(),
            b"patch_target",
            Element::new_item(b"hello".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert original item");

        // Now batch: Patch the target + insert a reference to it
        let ops = vec![
            QualifiedGroveDbOp::patch_op(
                vec![TEST_LEAF.to_vec()],
                b"patch_target".to_vec(),
                Element::new_item(b"world".to_vec()),
                0, // change_in_bytes
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"ref_to_patch".to_vec(),
                Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                    TEST_LEAF.to_vec(),
                    b"patch_target".to_vec(),
                ])),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch with Patch + ref should succeed");
    }

    // ===================================================================
    // Group 12: insert_item_element — empty reference error (L1653-1656)
    // ===================================================================

    #[test]
    fn test_batch_empty_reference_path_errors() {
        // Insert a reference with AbsolutePathReference(vec![]) — which resolves
        // to an empty path, triggering the "attempting to insert an empty reference"
        // error at L1652-1656.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"bad_ref".to_vec(),
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![])),
        )];

        let result = db.apply_batch(ops, None, None, grove_version).unwrap();
        assert!(result.is_err(), "empty reference path should fail");
        let err_msg = format!("{:?}", result.unwrap_err());
        assert!(
            err_msg.contains("empty reference"),
            "error should mention empty reference, got: {}",
            err_msg
        );
    }

    // ===================================================================
    // Group 13: process_reference — intermediate_reference_info branch
    //           (L1101-1112) — ref pointing to a RefreshReference target
    //           that itself has a reference_path_type with trust=true
    // ===================================================================

    #[test]
    fn test_batch_ref_to_trusted_refresh_reference() {
        // Setup: existing reference A -> item. In the batch:
        //   1. RefreshReference on A (trust=true, pointing to item)
        //   2. Insert new reference B -> A (which triggers
        //      follow_reference_get_value_hash, sees RefreshReference op,
        //      trust=true → passes intermediate_reference_info into
        //      process_reference, covering L1101-1112)
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert target item
        db.insert(
            [TEST_LEAF].as_ref(),
            b"item",
            Element::new_item(b"data".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item");

        // Insert existing reference A -> item
        db.insert(
            [TEST_LEAF].as_ref(),
            b"ref_a",
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"item".to_vec(),
            ])),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert ref_a");

        // Batch: refresh ref_a (trust=true) + insert ref_b -> ref_a
        let ops = vec![
            QualifiedGroveDbOp::refresh_reference_op(
                vec![TEST_LEAF.to_vec()],
                b"ref_a".to_vec(),
                ReferencePathType::AbsolutePathReference(vec![
                    TEST_LEAF.to_vec(),
                    b"item".to_vec(),
                ]),
                Some(2),
                None,
                true, // trust_refresh_reference
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"ref_b".to_vec(),
                Element::new_reference_with_hops(
                    ReferencePathType::AbsolutePathReference(vec![
                        TEST_LEAF.to_vec(),
                        b"ref_a".to_vec(),
                    ]),
                    Some(2),
                ),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch with trusted RefreshReference + ref should succeed");
    }

    #[test]
    fn test_batch_ref_to_untrusted_refresh_reference() {
        // Same as above but trust=false → intermediate_reference_info is None,
        // so it falls through to process_reference_with_hop_count_greater_than_one.
        // This covers the `else` branch at L1517-1518.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert target item
        db.insert(
            [TEST_LEAF].as_ref(),
            b"item2",
            Element::new_item(b"data2".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item2");

        // Insert existing reference A -> item2
        db.insert(
            [TEST_LEAF].as_ref(),
            b"ref_c",
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"item2".to_vec(),
            ])),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert ref_c");

        // Batch: refresh ref_c (trust=false) + insert ref_d -> ref_c
        let ops = vec![
            QualifiedGroveDbOp::refresh_reference_op(
                vec![TEST_LEAF.to_vec()],
                b"ref_c".to_vec(),
                ReferencePathType::AbsolutePathReference(vec![
                    TEST_LEAF.to_vec(),
                    b"item2".to_vec(),
                ]),
                Some(2),
                None,
                false, // trust_refresh_reference = false
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"ref_d".to_vec(),
                Element::new_reference_with_hops(
                    ReferencePathType::AbsolutePathReference(vec![
                        TEST_LEAF.to_vec(),
                        b"ref_c".to_vec(),
                    ]),
                    Some(3),
                ),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch with untrusted RefreshReference + ref should succeed");
    }

    // ===================================================================
    // Group 14: follow_reference_get_value_hash — ref pointing to a
    //           tree in an InsertOnly/Replace/Patch op (L1492-1506)
    // ===================================================================

    #[test]
    fn test_batch_ref_to_replace_tree_errors() {
        // A reference points to a key that is being Replace'd with a Tree element
        // → "references can not point to trees being updated" error.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // First insert a tree so Replace can find it
        db.insert(
            [TEST_LEAF].as_ref(),
            b"subtree",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert tree");

        let ops = vec![
            QualifiedGroveDbOp::replace_op(
                vec![TEST_LEAF.to_vec()],
                b"subtree".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"bad_ref".to_vec(),
                Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                    TEST_LEAF.to_vec(),
                    b"subtree".to_vec(),
                ])),
            ),
        ];

        let result = db.apply_batch(ops, None, None, grove_version).unwrap();
        assert!(result.is_err());
        let err_msg = format!("{:?}", result.unwrap_err());
        assert!(
            err_msg.contains("references can not point to trees"),
            "got: {}",
            err_msg
        );
    }

    // ===================================================================
    // Group 15: follow_reference_get_value_hash — ref pointing at a
    //           RefreshReference whose target is itself in the batch
    //           and points to a tree (L1509-1528 covering the full
    //           RefreshReference match arm)
    // ===================================================================

    #[test]
    fn test_batch_ref_to_refresh_ref_pointing_to_tree_errors() {
        // In this scenario a reference B points to ref_a, which is being refreshed
        // (trust=true). The trusted refresh says ref_a points to a tree element.
        // This should trigger the tree-pointing error in the element match within
        // the RefreshReference arm of follow_reference_get_value_hash.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert a tree to be the target of the reference chain
        db.insert(
            [TEST_LEAF].as_ref(),
            b"a_tree",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert tree");

        // Insert ref_a pointing to the tree (this is unusual but valid for insert)
        db.insert(
            [TEST_LEAF].as_ref(),
            b"ref_a2",
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"a_tree".to_vec(),
            ])),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert ref_a2");

        // Batch: refresh ref_a2 (trust=true, pointing to the tree) +
        // insert ref_b2 -> ref_a2 with hops > 1
        let ops = vec![
            QualifiedGroveDbOp::refresh_reference_op(
                vec![TEST_LEAF.to_vec()],
                b"ref_a2".to_vec(),
                ReferencePathType::AbsolutePathReference(vec![
                    TEST_LEAF.to_vec(),
                    b"a_tree".to_vec(),
                ]),
                Some(2),
                None,
                true,
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"ref_b2".to_vec(),
                Element::new_reference_with_hops(
                    ReferencePathType::AbsolutePathReference(vec![
                        TEST_LEAF.to_vec(),
                        b"ref_a2".to_vec(),
                    ]),
                    Some(3),
                ),
            ),
        ];

        // The trusted refresh resolves ref_a2's target. Since a_tree is a Tree,
        // process_reference will follow the chain and eventually hit the tree element
        // on disk. This should error because refs can't point to trees.
        let result = db.apply_batch(ops, None, None, grove_version).unwrap();
        assert!(
            result.is_err(),
            "ref chain through refresh to tree should fail"
        );
    }
}
