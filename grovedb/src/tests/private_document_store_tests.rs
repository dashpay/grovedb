//! PrivateDocumentStore integration tests
//!
//! Tests for PrivateDocumentStore as a GroveDB subtree type: an append-only
//! store of fixed-size opaque entries over a BulkAppendTree, with the
//! committed `{entry_size, chunk_power}` configuration bound into the state
//! root. Includes the fail-closed version-gating tests: every operation and
//! element creation must be rejected under GROVE_V3 and earlier.

use grovedb_merk::proofs::{query::SubqueryBranch, Query};
use grovedb_version::version::{v3::GROVE_V3, v4::GROVE_V4, GroveVersion};

use crate::{
    batch::QualifiedGroveDbOp,
    operations::delete::DeleteOptions,
    query_result_type::QueryResultType,
    tests::{common::EMPTY_PATH, make_empty_grovedb},
    Element, Error, GroveDb, PathQuery, SizedQuery,
};

/// Small chunk power for tests — epoch size = 2^2 = 4, triggers compaction
/// after 4 appends.
const TEST_CHUNK_POWER: u8 = 2;
/// Committed entry size for tests.
const TEST_ENTRY_SIZE: u32 = 16;

fn entry(byte: u8) -> Vec<u8> {
    vec![byte; TEST_ENTRY_SIZE as usize]
}

// ===========================================================================
// Element tests
// ===========================================================================

#[test]
fn test_insert_private_document_store_at_root() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"pds",
        Element::empty_private_document_store(TEST_ENTRY_SIZE, TEST_CHUNK_POWER)
            .expect("valid config"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert private document store at root");

    let element = db
        .get(EMPTY_PATH, b"pds", None, grove_version)
        .unwrap()
        .expect("should retrieve private document store");
    assert!(element.is_private_document_store());
    assert!(element.is_any_tree());
    assert!(element.uses_non_merk_data_storage());
    assert_eq!(element.non_merk_entry_count(), Some(0));

    // The whole database must pass the integrity walk.
    let issues = db
        .verify_grovedb(None, true, false, grove_version)
        .expect("verify_grovedb");
    assert!(issues.is_empty(), "issues: {:?}", issues);
}

#[test]
fn test_private_document_store_constructor_validation() {
    // entry_size 0 is rejected.
    assert!(Element::empty_private_document_store(0, TEST_CHUNK_POWER).is_err());
    // chunk_power outside 1..=16 is rejected.
    assert!(Element::empty_private_document_store(TEST_ENTRY_SIZE, 0).is_err());
    assert!(Element::empty_private_document_store(TEST_ENTRY_SIZE, 17).is_err());
    // Boundary values are accepted.
    assert!(Element::empty_private_document_store(1, 1).is_ok());
    assert!(Element::empty_private_document_store(u32::MAX, 16).is_ok());
}

#[test]
fn test_private_document_store_serialization_roundtrip() {
    let grove_version = GroveVersion::latest();
    let original = Element::new_private_document_store(100, 216, 12, Some(vec![7, 8, 9]));
    let bytes = original.serialize(grove_version).expect("serialize");
    // Discriminant 24 is the wire byte for PrivateDocumentStore.
    assert_eq!(bytes[0], 24, "bincode discriminant must be 24");
    let deserialized = Element::deserialize(&bytes, grove_version).expect("deserialize");
    assert_eq!(deserialized, original);

    // NonCounted wrapper round-trips as [15, 24, ...].
    let wrapped = Element::new_non_counted(original.clone()).expect("wrap");
    let wrapped_bytes = wrapped.serialize(grove_version).expect("serialize wrapped");
    assert_eq!(&wrapped_bytes[0..2], &[15, 24]);
    let unwrapped = Element::deserialize(&wrapped_bytes, grove_version).expect("deserialize");
    assert_eq!(unwrapped, wrapped);
    assert!(unwrapped.is_private_document_store());
}

// ===========================================================================
// Operation tests: insert (append), get_value, count
// ===========================================================================

/// Build a db with a parent tree at "root" and an empty store at
/// root/"docs".
fn make_db_with_store() -> crate::tests::TempGroveDb {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"root",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert root tree");

    db.insert(
        &[b"root"],
        b"docs",
        Element::empty_private_document_store(TEST_ENTRY_SIZE, TEST_CHUNK_POWER)
            .expect("valid config"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert private document store");

    db
}

#[test]
fn test_private_document_store_insert_get_count_roundtrip() {
    let grove_version = GroveVersion::latest();
    let db = make_db_with_store();

    // 10 appends span two completed chunks (epoch size 4) plus a partial
    // buffer, exercising both storage tiers.
    let mut roots = Vec::new();
    for i in 0..10u8 {
        let (state_root, position) = db
            .private_document_store_insert(&[b"root"], b"docs", entry(i), None, grove_version)
            .unwrap()
            .expect("append entry");
        assert_eq!(position, i as u64);
        roots.push(state_root);
    }
    // Every append must move the state root.
    for w in roots.windows(2) {
        assert_ne!(w[0], w[1]);
    }

    assert_eq!(
        db.private_document_store_count(&[b"root"], b"docs", None, grove_version)
            .unwrap()
            .expect("count"),
        10
    );

    for i in 0..10u8 {
        let value = db
            .private_document_store_get_value(&[b"root"], b"docs", i as u64, None, grove_version)
            .unwrap()
            .expect("get value");
        assert_eq!(value, Some(entry(i)), "position {}", i);
    }
    // Out-of-range read returns None.
    assert_eq!(
        db.private_document_store_get_value(&[b"root"], b"docs", 10, None, grove_version)
            .unwrap()
            .expect("get out of range"),
        None
    );

    // The whole database (including the value-hash binding of the store's
    // state root and the entry-size integrity walk) must verify.
    let issues = db
        .verify_grovedb(None, true, false, grove_version)
        .expect("verify_grovedb");
    assert!(issues.is_empty(), "issues: {:?}", issues);
}

#[test]
fn test_private_document_store_insert_rejects_wrong_entry_size() {
    let grove_version = GroveVersion::latest();
    let db = make_db_with_store();
    let root_before = db.root_hash(None, grove_version).unwrap().unwrap();

    for bad in [
        vec![0u8; TEST_ENTRY_SIZE as usize - 1],
        vec![0u8; TEST_ENTRY_SIZE as usize + 1],
        Vec::new(),
    ] {
        let result = db
            .private_document_store_insert(&[b"root"], b"docs", bad, None, grove_version)
            .unwrap();
        assert!(
            matches!(result, Err(Error::PrivateDocumentStoreError(_))),
            "expected entry-size rejection, got {:?}",
            result
        );
    }

    // Nothing was appended and the root hash is unchanged by rejections.
    assert_eq!(
        root_before,
        db.root_hash(None, grove_version).unwrap().unwrap(),
        "rejected appends must not change the grove root hash"
    );
    assert_eq!(
        db.private_document_store_count(&[b"root"], b"docs", None, grove_version)
            .unwrap()
            .expect("count"),
        0
    );
}

#[test]
fn test_private_document_store_ops_reject_wrong_element_type() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"plain",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert plain tree");

    assert!(matches!(
        db.private_document_store_insert(EMPTY_PATH, b"plain", entry(0), None, grove_version)
            .unwrap(),
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        db.private_document_store_get_value(EMPTY_PATH, b"plain", 0, None, grove_version)
            .unwrap(),
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        db.private_document_store_count(EMPTY_PATH, b"plain", None, grove_version)
            .unwrap(),
        Err(Error::InvalidInput(_))
    ));
}

#[test]
fn test_private_document_store_root_hash_changes_and_persists() {
    let grove_version = GroveVersion::latest();
    let db = make_db_with_store();

    let root_before = db.root_hash(None, grove_version).unwrap().unwrap();
    db.private_document_store_insert(&[b"root"], b"docs", entry(1), None, grove_version)
        .unwrap()
        .expect("append");
    let root_after = db.root_hash(None, grove_version).unwrap().unwrap();
    assert_ne!(
        root_before, root_after,
        "append must change the grove root hash"
    );

    // The updated element reflects the new count.
    let element = db
        .get(&[b"root"], b"docs", None, grove_version)
        .unwrap()
        .expect("get element");
    assert_eq!(element.non_merk_entry_count(), Some(1));
}

// ===========================================================================
// Batch tests
// ===========================================================================

#[test]
fn test_private_document_store_batch_create_and_append() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Create the parent and the empty store in one batch.
    let ops = vec![
        QualifiedGroveDbOp::insert_or_replace_op(vec![], b"root".to_vec(), Element::empty_tree()),
        QualifiedGroveDbOp::insert_or_replace_op(
            vec![b"root".to_vec()],
            b"docs".to_vec(),
            Element::empty_private_document_store(TEST_ENTRY_SIZE, TEST_CHUNK_POWER)
                .expect("valid config"),
        ),
    ];
    db.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("apply creation batch");

    // Append 6 entries via batch ops (spans one compaction at epoch size 4).
    let ops = (0..6u8)
        .map(|i| {
            QualifiedGroveDbOp::private_document_store_insert_op(
                vec![b"root".to_vec(), b"docs".to_vec()],
                entry(i),
            )
        })
        .collect();
    db.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("apply append batch");

    assert_eq!(
        db.private_document_store_count(&[b"root"], b"docs", None, grove_version)
            .unwrap()
            .expect("count"),
        6
    );
    for i in 0..6u8 {
        assert_eq!(
            db.private_document_store_get_value(&[b"root"], b"docs", i as u64, None, grove_version)
                .unwrap()
                .expect("get"),
            Some(entry(i))
        );
    }

    let issues = db
        .verify_grovedb(None, true, false, grove_version)
        .expect("verify_grovedb");
    assert!(issues.is_empty(), "issues: {:?}", issues);

    // Batch appends must produce the same root as the equivalent direct
    // appends.
    let direct_db = make_db_with_store();
    for i in 0..6u8 {
        direct_db
            .private_document_store_insert(&[b"root"], b"docs", entry(i), None, grove_version)
            .unwrap()
            .expect("direct append");
    }
    assert_eq!(
        db.root_hash(None, grove_version).unwrap().unwrap(),
        direct_db.root_hash(None, grove_version).unwrap().unwrap(),
        "batch and direct appends must converge to the same root hash"
    );
}

#[test]
fn test_private_document_store_batch_rejects_wrong_entry_size() {
    let grove_version = GroveVersion::latest();
    let db = make_db_with_store();

    let ops = vec![QualifiedGroveDbOp::private_document_store_insert_op(
        vec![b"root".to_vec(), b"docs".to_vec()],
        vec![0u8; TEST_ENTRY_SIZE as usize + 1],
    )];
    let result = db.apply_batch(ops, None, None, grove_version).unwrap();
    assert!(
        matches!(result, Err(Error::PrivateDocumentStoreError(_))),
        "expected entry-size rejection, got {:?}",
        result
    );
    assert_eq!(
        db.private_document_store_count(&[b"root"], b"docs", None, grove_version)
            .unwrap()
            .expect("count"),
        0
    );
}

#[test]
fn test_private_document_store_batch_rejects_non_empty_element_insert() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Inserting a PrivateDocumentStore element claiming a non-zero count is
    // rejected: the batch write binds the empty state root, so a non-zero
    // claim would corrupt the root binding.
    let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
        vec![],
        b"docs".to_vec(),
        Element::new_private_document_store(5, TEST_ENTRY_SIZE, TEST_CHUNK_POWER, None),
    )];
    let result = db.apply_batch(ops, None, None, grove_version).unwrap();
    assert!(
        matches!(result, Err(Error::InvalidBatchOperation(_))),
        "expected non-empty rejection, got {:?}",
        result
    );

    // Same for an invalid config built without the checked constructors.
    let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
        vec![],
        b"docs".to_vec(),
        Element::new_private_document_store(0, 0, TEST_CHUNK_POWER, None),
    )];
    let result = db.apply_batch(ops, None, None, grove_version).unwrap();
    assert!(
        matches!(result, Err(Error::InvalidBatchOperation(_))),
        "expected config rejection, got {:?}",
        result
    );
}

#[test]
fn test_private_document_store_direct_insert_rejects_non_empty_element() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    let result = db
        .insert(
            EMPTY_PATH,
            b"docs",
            Element::new_private_document_store(5, TEST_ENTRY_SIZE, TEST_CHUNK_POWER, None),
            None,
            None,
            grove_version,
        )
        .unwrap();
    assert!(
        matches!(result, Err(Error::InvalidCodeExecution(_))),
        "expected non-empty rejection, got {:?}",
        result
    );

    // A caller-built element with an invalid config (bypassing the checked
    // constructors) is rejected by the insert path as well.
    for bad in [
        Element::new_private_document_store(0, 0, TEST_CHUNK_POWER, None),
        Element::new_private_document_store(0, TEST_ENTRY_SIZE, 0, None),
        Element::new_private_document_store(0, TEST_ENTRY_SIZE, 17, None),
    ] {
        let result = db
            .insert(EMPTY_PATH, b"docs", bad, None, None, grove_version)
            .unwrap();
        assert!(
            matches!(result, Err(Error::InvalidInput(_))),
            "expected config rejection, got {:?}",
            result
        );
    }
}

#[test]
fn test_private_document_store_rejects_child_element_inserts() {
    // Immutability is enforced by the type: the store's (always-empty) Merk
    // may never hold child elements. Both the generic direct insert and the
    // batch insert into the store's path must be rejected at the merk layer.
    let grove_version = GroveVersion::latest();
    let db = make_db_with_store();

    let result = db
        .insert(
            &[b"root".as_slice(), b"docs".as_slice()],
            b"child",
            Element::new_item(b"data".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap();
    match &result {
        Err(e) => assert!(
            e.to_string()
                .contains("private document stores cannot hold child elements"),
            "unexpected rejection error: {}",
            e
        ),
        Ok(_) => panic!("direct child insert into a store must be rejected"),
    }

    let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
        vec![b"root".to_vec(), b"docs".to_vec()],
        b"child".to_vec(),
        Element::new_item(b"data".to_vec()),
    )];
    let result = db.apply_batch(ops, None, None, grove_version).unwrap();
    assert!(
        result.is_err(),
        "batch child insert into a store must be rejected, got {:?}",
        result
    );

    // is_empty_tree on the store path still works (reads the element count
    // from the parent).
    assert!(db
        .is_empty_tree(
            &[b"root".as_slice(), b"docs".as_slice()],
            None,
            grove_version
        )
        .unwrap()
        .expect("is_empty_tree"));
}

// ===========================================================================
// Delete tests
// ===========================================================================

#[test]
fn test_private_document_store_delete() {
    let grove_version = GroveVersion::latest();
    let db = make_db_with_store();

    for i in 0..5u8 {
        db.private_document_store_insert(&[b"root"], b"docs", entry(i), None, grove_version)
            .unwrap()
            .expect("append");
    }

    // A populated store requires allow_deleting_non_empty_trees.
    let result = db
        .delete(
            &[b"root"],
            b"docs",
            Some(DeleteOptions {
                allow_deleting_non_empty_trees: false,
                deleting_non_empty_trees_returns_error: true,
                ..Default::default()
            }),
            None,
            grove_version,
        )
        .unwrap();
    assert!(result.is_err(), "deleting populated store must error");

    db.delete(
        &[b"root"],
        b"docs",
        Some(DeleteOptions {
            allow_deleting_non_empty_trees: true,
            deleting_non_empty_trees_returns_error: false,
            ..Default::default()
        }),
        None,
        grove_version,
    )
    .unwrap()
    .expect("delete populated store");

    assert!(matches!(
        db.get(&[b"root"], b"docs", None, grove_version).unwrap(),
        Err(Error::PathKeyNotFound(_))
    ));

    // The store's non-Merk data namespace (buffer entries, chunk blobs, MMR
    // nodes) must be reclaimed by the delete — not just the parent element.
    {
        use grovedb_storage::{RawIterator, Storage, StorageContext};
        let tx = db.start_transaction();
        let ctx = db
            .db
            .get_transactional_storage_context(
                grovedb_path::SubtreePath::from([b"root".as_slice(), b"docs".as_slice()].as_ref()),
                None,
                &tx,
            )
            .unwrap();
        let mut iter = ctx.raw_iter();
        iter.seek_to_first().unwrap();
        assert!(
            !iter.valid().unwrap(),
            "deleted store left orphaned rows in its data namespace"
        );
    }

    // A store recreated at the same path starts from scratch: appends begin
    // at position 0 and nothing from the deleted store is visible.
    db.insert(
        &[b"root"],
        b"docs",
        Element::empty_private_document_store(TEST_ENTRY_SIZE, TEST_CHUNK_POWER)
            .expect("valid config"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("recreate store");
    let (_, position) = db
        .private_document_store_insert(&[b"root"], b"docs", entry(99), None, grove_version)
        .unwrap()
        .expect("append to recreated store");
    assert_eq!(position, 0);
    assert_eq!(
        db.private_document_store_get_value(&[b"root"], b"docs", 0, None, grove_version)
            .unwrap()
            .expect("get"),
        Some(entry(99))
    );

    let issues = db
        .verify_grovedb(None, true, false, grove_version)
        .expect("verify_grovedb");
    assert!(issues.is_empty(), "issues: {:?}", issues);
}

// ===========================================================================
// Proof tests (terminal binding only — range reads are a follow-up)
// ===========================================================================

/// Query the store's key itself (no subquery) and verify the V1 proof. This
/// exercises the terminal non-Merk binding: the store's config-bound state
/// root is carried as the node's child hash and checked against the parent
/// commit.
fn prove_and_verify_store_element(db: &GroveDb, expected_count: u64) {
    let grove_version = GroveVersion::latest();
    let path_query = PathQuery {
        path: vec![b"root".to_vec()],
        query: SizedQuery {
            query: Query {
                items: vec![grovedb_merk::proofs::query::QueryItem::Key(
                    b"docs".to_vec(),
                )],
                default_subquery_branch: SubqueryBranch {
                    subquery_path: None,
                    subquery: None,
                },
                left_to_right: true,
                conditional_subquery_branches: None,
                add_parent_tree_on_subquery: false,
            },
            limit: None,
            offset: None,
        },
    };

    let proof_bytes = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .expect("generate V1 proof for store element");

    let (root_hash, result_set) = GroveDb::verify_query_with_options(
        &proof_bytes,
        &path_query,
        grovedb_merk::proofs::query::VerifyOptions {
            absence_proofs_for_non_existing_searched_keys: false,
            verify_proof_succinctness: false,
            include_empty_trees_in_result: true,
        },
        grove_version,
    )
    .expect("verify V1 proof for store element");

    let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
    assert_eq!(root_hash, expected_root, "root hash should match");
    assert_eq!(result_set.len(), 1, "store element should be in results");
    let element = result_set[0]
        .2
        .clone()
        .expect("proved element should be present");
    match element {
        Element::PrivateDocumentStore(total_count, entry_size, chunk_power, _) => {
            assert_eq!(total_count, expected_count);
            assert_eq!(entry_size, TEST_ENTRY_SIZE);
            assert_eq!(chunk_power, TEST_CHUNK_POWER);
        }
        other => panic!("expected PrivateDocumentStore, got {:?}", other.type_str()),
    }
}

#[test]
fn test_private_document_store_prove_element_empty_and_populated() {
    let grove_version = GroveVersion::latest();
    let db = make_db_with_store();

    // Empty store: the terminal binding uses the config-parametrized empty
    // state root.
    prove_and_verify_store_element(&db, 0);

    // Populated store (across a compaction boundary).
    for i in 0..5u8 {
        db.private_document_store_insert(&[b"root"], b"docs", entry(i), None, grove_version)
            .unwrap()
            .expect("append");
    }
    prove_and_verify_store_element(&db, 5);
}

#[test]
fn test_private_document_store_subquery_proofs_not_supported() {
    let grove_version = GroveVersion::latest();
    let db = make_db_with_store();
    db.private_document_store_insert(&[b"root"], b"docs", entry(1), None, grove_version)
        .unwrap()
        .expect("append");

    let mut inner_query = Query::new();
    inner_query.insert_all();
    let path_query = PathQuery {
        path: vec![b"root".to_vec()],
        query: SizedQuery {
            query: Query {
                items: vec![grovedb_merk::proofs::query::QueryItem::Key(
                    b"docs".to_vec(),
                )],
                default_subquery_branch: SubqueryBranch {
                    subquery_path: None,
                    subquery: Some(inner_query.into()),
                },
                left_to_right: true,
                conditional_subquery_branches: None,
                add_parent_tree_on_subquery: false,
            },
            limit: None,
            offset: None,
        },
    };

    let result = db.prove_query(&path_query, None, grove_version).unwrap();
    assert!(
        matches!(result, Err(Error::NotSupported(_))),
        "subquery proofs into a PrivateDocumentStore are not implemented yet, got {:?}",
        result
    );
}

// ===========================================================================
// Query tests (non-proof reads)
// ===========================================================================

#[test]
fn test_private_document_store_path_query_rejects_tree_result() {
    let grove_version = GroveVersion::latest();
    let db = make_db_with_store();

    // Item-value queries cannot return trees.
    let path_query = PathQuery::new_unsized(
        vec![b"root".to_vec()],
        Query::new_single_key(b"docs".to_vec()),
    );
    let result = db
        .query_item_value(&path_query, true, true, true, None, grove_version)
        .unwrap();
    assert!(matches!(result, Err(Error::InvalidQuery(_))));

    // The item-or-sum variant rejects stores too.
    let result = db
        .query_item_value_or_sum(&path_query, true, true, true, None, grove_version)
        .unwrap();
    assert!(matches!(result, Err(Error::InvalidQuery(_))));

    // Raw element queries return the store element itself.
    let (elements, _) = db
        .query_raw(
            &path_query,
            true,
            true,
            true,
            QueryResultType::QueryElementResultType,
            None,
            grove_version,
        )
        .unwrap()
        .expect("raw query");
    assert_eq!(elements.len(), 1);
}

// ===========================================================================
// Version gating: fail closed on GROVE_V3 and earlier
// ===========================================================================

#[test]
fn test_private_document_store_version_slots_pinned() {
    // The gate itself: all slots 0 on V3 (fail closed), 1 on V4.
    let v3 = &GROVE_V3.grovedb_versions.operations.private_document_store;
    assert_eq!(v3.element_creation, 0);
    assert_eq!(v3.insert, 0);
    assert_eq!(v3.get_value, 0);
    assert_eq!(v3.count, 0);
    let v4 = &GROVE_V4.grovedb_versions.operations.private_document_store;
    assert_eq!(v4.element_creation, 1);
    assert_eq!(v4.insert, 1);
    assert_eq!(v4.get_value, 1);
    assert_eq!(v4.count, 1);
}

#[test]
fn test_private_document_store_element_creation_rejected_under_v3() {
    let db = make_empty_grovedb();

    // Direct insert of the element fails closed under V3.
    let result = db
        .insert(
            EMPTY_PATH,
            b"docs",
            Element::empty_private_document_store(TEST_ENTRY_SIZE, TEST_CHUNK_POWER)
                .expect("valid config"),
            None,
            None,
            &GROVE_V3,
        )
        .unwrap();
    assert!(
        matches!(result, Err(Error::VersionError(_))),
        "V3 direct insert must fail closed, got {:?}",
        result
    );

    // Batch insert of the element fails closed under V3.
    let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
        vec![],
        b"docs".to_vec(),
        Element::empty_private_document_store(TEST_ENTRY_SIZE, TEST_CHUNK_POWER)
            .expect("valid config"),
    )];
    let result = db.apply_batch(ops, None, None, &GROVE_V3).unwrap();
    assert!(
        matches!(result, Err(Error::VersionError(_))),
        "V3 batch insert must fail closed, got {:?}",
        result
    );

    // Under V4 the same element inserts fine (registered latest is V4).
    db.insert(
        EMPTY_PATH,
        b"docs",
        Element::empty_private_document_store(TEST_ENTRY_SIZE, TEST_CHUNK_POWER)
            .expect("valid config"),
        None,
        None,
        &GROVE_V4,
    )
    .unwrap()
    .expect("V4 insert works");
}

#[test]
fn test_private_document_store_operations_rejected_under_v3() {
    // Build the store under V4 (latest), then attempt V3 operations on it.
    let db = make_db_with_store();

    let result = db
        .private_document_store_insert(&[b"root"], b"docs", entry(0), None, &GROVE_V3)
        .unwrap();
    assert!(
        matches!(result, Err(Error::VersionError(_))),
        "V3 insert op must fail closed, got {:?}",
        result
    );

    let result = db
        .private_document_store_get_value(&[b"root"], b"docs", 0, None, &GROVE_V3)
        .unwrap();
    assert!(
        matches!(result, Err(Error::VersionError(_))),
        "V3 get_value op must fail closed, got {:?}",
        result
    );

    let result = db
        .private_document_store_count(&[b"root"], b"docs", None, &GROVE_V3)
        .unwrap();
    assert!(
        matches!(result, Err(Error::VersionError(_))),
        "V3 count op must fail closed, got {:?}",
        result
    );

    // Batch append op fails closed under V3.
    let ops = vec![QualifiedGroveDbOp::private_document_store_insert_op(
        vec![b"root".to_vec(), b"docs".to_vec()],
        entry(0),
    )];
    let result = db.apply_batch(ops, None, None, &GROVE_V3).unwrap();
    assert!(
        matches!(result, Err(Error::VersionError(_))),
        "V3 batch append must fail closed, got {:?}",
        result
    );

    // Nothing was appended by the rejected calls.
    assert_eq!(
        db.private_document_store_count(&[b"root"], b"docs", None, GroveVersion::latest())
            .unwrap()
            .expect("count under latest"),
        0
    );
}

// ===========================================================================
// State-root binding tests
// ===========================================================================

#[test]
fn test_private_document_store_config_binds_root_hash() {
    // Two stores with identical entries but different committed configs must
    // produce different grove root hashes (the config is bound into the
    // state root even for empty stores).
    let grove_version = GroveVersion::latest();

    let build = |entry_size: u32, chunk_power: u8| {
        let db = make_empty_grovedb();
        db.insert(
            EMPTY_PATH,
            b"docs",
            Element::empty_private_document_store(entry_size, chunk_power).expect("valid config"),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert store");
        db.root_hash(None, grove_version).unwrap().unwrap()
    };

    let base = build(TEST_ENTRY_SIZE, TEST_CHUNK_POWER);
    // NOTE: entry_size and chunk_power are serialized in the element bytes,
    // so the value hash differs too — the stronger claim (the *child hash*
    // alone differs) is covered by the crate-level state-root tests. Here we
    // pin that config changes are visible at the grove root.
    assert_ne!(base, build(TEST_ENTRY_SIZE + 1, TEST_CHUNK_POWER));
    assert_ne!(base, build(TEST_ENTRY_SIZE, TEST_CHUNK_POWER + 1));
}

#[test]
fn test_private_document_store_empty_root_constant_matches_insert_binding() {
    // The empty-root helper must agree with what the insert path binds:
    // verify_grovedb recomputes the binding from the helper, so a clean
    // verify after a fresh insert proves the two paths agree.
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();
    db.insert(
        EMPTY_PATH,
        b"docs",
        Element::empty_private_document_store(TEST_ENTRY_SIZE, TEST_CHUNK_POWER)
            .expect("valid config"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert store");
    let issues = db
        .verify_grovedb(None, true, false, grove_version)
        .expect("verify_grovedb");
    assert!(issues.is_empty(), "issues: {:?}", issues);
}

// ===========================================================================
// Coverage: v0 insert path, batch policy branches, op metadata
// ===========================================================================

/// Exercise the v0 `add_element_on_transaction` arm for
/// PrivateDocumentStore. No registered version pairs the v0 insert
/// implementation with an enabled PDS family (V1..V3 fail closed, V4 uses
/// v1), so drive it with a custom version: V4 with the insert
/// implementation slot dialed back to 0.
#[test]
fn test_private_document_store_insert_v0_element_path() {
    let mut custom = GROVE_V4.clone();
    custom
        .grovedb_versions
        .operations
        .insert
        .add_element_on_transaction = 0;
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"docs",
        Element::empty_private_document_store(TEST_ENTRY_SIZE, TEST_CHUNK_POWER)
            .expect("valid config"),
        None,
        None,
        &custom,
    )
    .unwrap()
    .expect("v0 insert of empty store works");

    // The v0 arm enforces the same emptiness and config validation.
    let result = db
        .insert(
            EMPTY_PATH,
            b"docs2",
            Element::new_private_document_store(5, TEST_ENTRY_SIZE, TEST_CHUNK_POWER, None),
            None,
            None,
            &custom,
        )
        .unwrap();
    assert!(matches!(result, Err(Error::InvalidCodeExecution(_))));

    let result = db
        .insert(
            EMPTY_PATH,
            b"docs3",
            Element::new_private_document_store(0, 0, TEST_CHUNK_POWER, None),
            None,
            None,
            &custom,
        )
        .unwrap();
    assert!(matches!(result, Err(Error::InvalidInput(_))));

    // The binding written by the v0 arm matches what verify_grovedb
    // recomputes (same config-parametrized empty root as the v1 arm).
    let issues = db
        .verify_grovedb(None, true, false, GroveVersion::latest())
        .expect("verify_grovedb");
    assert!(issues.is_empty(), "issues: {:?}", issues);
}

#[test]
fn test_private_document_store_batch_insert_if_not_exists() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    let store_element = || {
        Element::empty_private_document_store(TEST_ENTRY_SIZE, TEST_CHUNK_POWER)
            .expect("valid config")
    };

    // First InsertIfNotExists creates the store.
    let ops = vec![QualifiedGroveDbOp::insert_if_not_exists_op(
        vec![],
        b"docs".to_vec(),
        store_element(),
    )];
    db.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("first insert_if_not_exists creates the store");

    // A second InsertIfNotExists over the same key hits the existence check
    // and errors (validate_insertion_does_not_override defaults to erroring
    // for InsertIfNotExists in batches).
    let ops = vec![QualifiedGroveDbOp::insert_if_not_exists_op(
        vec![],
        b"docs".to_vec(),
        store_element(),
    )];
    let result = db.apply_batch(ops, None, None, grove_version).unwrap();
    assert!(
        matches!(result, Err(Error::InvalidBatchOperation(_))),
        "duplicate insert_if_not_exists must be rejected, got {:?}",
        result
    );

    // The store is intact.
    assert_eq!(
        db.private_document_store_count(EMPTY_PATH, b"docs", None, grove_version)
            .unwrap()
            .expect("count"),
        0
    );
}

#[test]
fn test_private_document_store_reference_to_updated_store_rejected() {
    let grove_version = GroveVersion::latest();
    let db = make_db_with_store();

    // A reference in the same batch pointing at a store that receives
    // appends must be rejected: references cannot point to trees being
    // updated.
    let ops = vec![
        QualifiedGroveDbOp::private_document_store_insert_op(
            vec![b"root".to_vec(), b"docs".to_vec()],
            entry(1),
        ),
        QualifiedGroveDbOp::insert_or_replace_op(
            vec![b"root".to_vec()],
            b"link".to_vec(),
            Element::new_reference(
                crate::reference_path::ReferencePathType::AbsolutePathReference(vec![
                    b"root".to_vec(),
                    b"docs".to_vec(),
                ]),
            ),
        ),
    ];
    let result = db.apply_batch(ops, None, None, grove_version).unwrap();
    assert!(
        matches!(result, Err(Error::InvalidBatchOperation(_))),
        "reference to a store being appended to must be rejected, got {:?}",
        result
    );
}

#[test]
fn test_private_document_store_op_metadata() {
    // The sort tag is consensus-relevant for batch op ordering; pin it.
    let op = crate::batch::GroveOp::PrivateDocumentStoreInsert { entry: entry(0) };
    assert_eq!(op.to_u8(), 19);
    assert!(!op.can_mutate_child_count());

    // NonMerkTreeMeta round-trips the element state.
    let meta = crate::batch::NonMerkTreeMeta::PrivateDocumentStore {
        total_count: 7,
        entry_size: TEST_ENTRY_SIZE,
        chunk_power: TEST_CHUNK_POWER,
    };
    assert_eq!(
        meta.to_tree_type(),
        grovedb_merk::tree_type::TreeType::PrivateDocumentStore(TEST_CHUNK_POWER)
    );
    assert_eq!(meta.count(), 7);
    assert_eq!(
        meta.to_element(Some(vec![1])),
        Element::new_private_document_store(7, TEST_ENTRY_SIZE, TEST_CHUNK_POWER, Some(vec![1]))
    );
}

#[test]
fn test_private_document_store_apply_without_batching() {
    // The non-batch fallback path dispatches PrivateDocumentStoreInsert ops
    // to the direct typed insert.
    let grove_version = GroveVersion::latest();
    let db = make_db_with_store();

    let ops = vec![
        QualifiedGroveDbOp::private_document_store_insert_op(
            vec![b"root".to_vec(), b"docs".to_vec()],
            entry(1),
        ),
        QualifiedGroveDbOp::private_document_store_insert_op(
            vec![b"root".to_vec(), b"docs".to_vec()],
            entry(2),
        ),
    ];
    db.apply_operations_without_batching(ops, None, None, grove_version)
        .unwrap()
        .expect("apply without batching");

    assert_eq!(
        db.private_document_store_count(&[b"root"], b"docs", None, grove_version)
            .unwrap()
            .expect("count"),
        2
    );
    assert_eq!(
        db.private_document_store_get_value(&[b"root"], b"docs", 1, None, grove_version)
            .unwrap()
            .expect("get"),
        Some(entry(2))
    );

    let issues = db
        .verify_grovedb(None, true, false, grove_version)
        .expect("verify_grovedb");
    assert!(issues.is_empty(), "issues: {:?}", issues);
}

#[test]
fn test_private_document_store_element_display_and_visualize() {
    let element = Element::new_private_document_store(3, TEST_ENTRY_SIZE, TEST_CHUNK_POWER, None);
    let display = format!("{}", element);
    assert!(
        display.contains("PrivateDocumentStore")
            && display.contains("entry_size: 16")
            && display.contains("chunk_power: 2"),
        "unexpected display: {}",
        display
    );
    assert_eq!(element.type_str(), "private_document_store");
    assert_eq!(
        Element::new_non_counted(element).expect("wrap").type_str(),
        "non_counted private_document_store"
    );
}

#[test]
fn test_private_document_store_v0_prover_rejects_subqueries() {
    use grovedb_version::version::v2::GROVE_V2;

    // Build the store under the latest version, then generate proofs with
    // GROVE_V2 (whose prover is the locked V0 wire format). Subqueries into
    // the store must be rejected by the V0 dispatch...
    let grove_version = GroveVersion::latest();
    let db = make_db_with_store();
    db.private_document_store_insert(&[b"root"], b"docs", entry(1), None, grove_version)
        .unwrap()
        .expect("append");

    let mut inner_query = Query::new();
    inner_query.insert_all();
    let subquery = PathQuery {
        path: vec![b"root".to_vec()],
        query: SizedQuery {
            query: Query {
                items: vec![grovedb_merk::proofs::query::QueryItem::Key(
                    b"docs".to_vec(),
                )],
                default_subquery_branch: SubqueryBranch {
                    subquery_path: None,
                    subquery: Some(inner_query.into()),
                },
                left_to_right: true,
                conditional_subquery_branches: None,
                add_parent_tree_on_subquery: false,
            },
            limit: None,
            offset: None,
        },
    };
    let result = db.prove_query(&subquery, None, &GROVE_V2).unwrap();
    assert!(
        matches!(result, Err(Error::NotSupported(_))),
        "V0 subquery into a store must be rejected, got {:?}",
        result
    );

    // ...while a terminal query for the element itself still proves and
    // verifies (the node passes through as a result, V0 shape unchanged).
    let terminal = PathQuery::new_unsized(
        vec![b"root".to_vec()],
        Query::new_single_key(b"docs".to_vec()),
    );
    let proof_bytes = db
        .prove_query(&terminal, None, &GROVE_V2)
        .unwrap()
        .expect("V0 terminal proof over a store element generates");
    let (root_hash, result_set) = GroveDb::verify_query_with_options(
        &proof_bytes,
        &terminal,
        grovedb_merk::proofs::query::VerifyOptions {
            absence_proofs_for_non_existing_searched_keys: false,
            verify_proof_succinctness: false,
            include_empty_trees_in_result: true,
        },
        &GROVE_V2,
    )
    .expect("verify V0 terminal proof");
    assert_eq!(
        root_hash,
        db.root_hash(None, grove_version).unwrap().unwrap(),
        "V0 proof must bind the current grove root"
    );
    assert_eq!(result_set.len(), 1);
}
