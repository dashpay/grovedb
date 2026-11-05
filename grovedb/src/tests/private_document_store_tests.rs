//! PrivateDocumentStore integration tests
//!
//! Tests for PrivateDocumentStore as a GroveDB subtree type: an append-only
//! store of fixed-size opaque entries over a BulkAppendTree, with the
//! committed `{entry_size, chunk_power}` configuration bound into the state
//! root. Includes the fail-closed version-gating tests: every operation and
//! element creation must be rejected under GROVE_V3 and earlier.

use grovedb_costs::CostContext;
use grovedb_merk::proofs::{query::SubqueryBranch, Query};
use grovedb_version::version::{v3::GROVE_V3, v4::GROVE_V4, v5::GROVE_V5, GroveVersion};

use crate::{
    batch::QualifiedGroveDbOp,
    operations::{
        delete::DeleteOptions,
        proof::{GroveDBProof, GroveDBProofV1, ProofBytes},
    },
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
    // entry_size is capped at u16::MAX so the worst-case compaction blob
    // (2^16 * entry_size) stays representable in the u32 added_bytes field,
    // keeping the worst-case storage estimate a real upper bound.
    assert!(Element::empty_private_document_store(u16::MAX as u32 + 1, 4).is_err());
    assert!(Element::empty_private_document_store(u32::MAX, 4).is_err());
    // Boundary values are accepted.
    assert!(Element::empty_private_document_store(1, 1).is_ok());
    assert!(Element::empty_private_document_store(u16::MAX as u32, 16).is_ok());
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
    assert!(matches!(
        db.private_document_store_get_range(EMPTY_PATH, b"plain", 0, 10, None, grove_version)
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
                read_mode: None,
                limit: None,
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

/// The path query a reader proves a store's entries with: the store at
/// `[root]/docs`, and every position below it.
fn store_subquery_path_query() -> PathQuery {
    let mut inner_query = Query::new();
    inner_query.insert_all();
    PathQuery {
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
                read_mode: None,
                limit: None,
            },
            limit: None,
            offset: None,
        },
    }
}

/// A store at `[root]/docs` holding `count` entries, entry `i` filled with
/// byte `i + 1`: two completed chunks and two buffered entries for 10.
fn make_db_with_entries(count: u8) -> crate::tests::TempGroveDb {
    let db = make_db_with_store();
    for i in 0..count {
        db.private_document_store_insert(
            &[b"root"],
            b"docs",
            entry(i + 1),
            None,
            GroveVersion::latest(),
        )
        .unwrap()
        .expect("append");
    }
    db
}

#[test]
fn test_private_document_store_subquery_proofs_verify() {
    let grove_version = GroveVersion::latest();
    let db = make_db_with_entries(10);
    let path_query = store_subquery_path_query();

    let proof = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .expect("prove every entry of the store");
    let (root_hash, results) = GroveDb::verify_query_with_options(
        &proof,
        &path_query,
        grovedb_merk::proofs::query::VerifyOptions {
            absence_proofs_for_non_existing_searched_keys: false,
            verify_proof_succinctness: false,
            include_empty_trees_in_result: false,
        },
        grove_version,
    )
    .expect("verify the store's entries");

    assert_eq!(
        root_hash,
        db.root_hash(None, grove_version).unwrap().unwrap()
    );
    let entries: Vec<(u64, Vec<u8>)> = results
        .into_iter()
        .map(|(path, key, element)| {
            assert_eq!(path, vec![b"root".to_vec(), b"docs".to_vec()]);
            let position = u64::from_be_bytes(key.try_into().expect("8-byte position key"));
            match element.expect("a proved entry") {
                Element::Item(value, _) => (position, value),
                other => panic!("expected an item, got {:?}", other.type_str()),
            }
        })
        .collect();
    let expected: Vec<(u64, Vec<u8>)> = (0..10u8).map(|i| (i as u64, entry(i + 1))).collect();
    assert_eq!(entries, expected);
}

#[test]
fn test_private_document_store_subquery_proofs_refused_at_grove_v4() {
    // GroveDB 6.1.1 shipped GROVE_V4 without a store's range proofs.
    let db = make_db_with_entries(5);
    let path_query = store_subquery_path_query();
    let without = &GROVE_V4;

    let result = db.prove_query(&path_query, None, without).unwrap();
    assert!(
        matches!(result, Err(Error::NotSupported(_))),
        "proving into a store must fail without the slot, got {:?}",
        result
    );

    // A proof made where the slot is set does not verify where it is not.
    let proof = db
        .prove_query(&path_query, None, GroveVersion::latest())
        .unwrap()
        .expect("prove under latest");
    let result = GroveDb::verify_query_with_options(
        &proof,
        &path_query,
        grovedb_merk::proofs::query::VerifyOptions {
            absence_proofs_for_non_existing_searched_keys: false,
            verify_proof_succinctness: false,
            include_empty_trees_in_result: false,
        },
        without,
    );
    assert!(
        matches!(result, Err(Error::NotSupported(_))),
        "verifying a store's lower layer must fail without the slot, got {:?}",
        result
    );

    let result = db
        .prove_bulk_position_range(vec![b"root".to_vec()], b"docs", 0, 5, None, without)
        .unwrap();
    assert!(
        matches!(result, Err(Error::InvalidInput(_))),
        "prove_bulk_position_range on a store must fail without the slot, got {:?}",
        result
    );

    let result = db
        .private_document_store_get_range(&[b"root"], b"docs", 0, 5, None, without)
        .unwrap();
    assert!(
        matches!(result, Err(Error::VersionError(_))),
        "a store's range read must fail closed without the slot, got {:?}",
        result
    );
}

/// Ranges inside a chunk, across chunks, across the last chunk and the
/// buffer, inside the buffer, past the end and empty: the unproved read and
/// the proved page agree, the proof binds the database root, and a page
/// stops at the store's count.
#[test]
fn test_private_document_store_range_reads_and_proofs() {
    let grove_version = GroveVersion::latest();
    let db = make_db_with_entries(10);
    let root = db.root_hash(None, grove_version).unwrap().unwrap();

    for (start, limit) in [
        (0u64, 3u16),
        (2, 5),
        (6, 4),
        (8, 2),
        (8, 20),
        (0, 10),
        (0, u16::MAX),
        (9, 1),
        (10, 5),
        (100, 5),
        (3, 0),
    ] {
        let expected_end = start.saturating_add(limit as u64).min(10).max(start);
        let expected: Vec<(u64, Vec<u8>)> = (start..expected_end)
            .map(|position| (position, entry(position as u8 + 1)))
            .collect();

        let page = db
            .private_document_store_get_range(
                &[b"root"],
                b"docs",
                start,
                limit,
                None,
                grove_version,
            )
            .unwrap()
            .expect("read the range");
        assert_eq!(page.entries, expected, "unproved [{start}, +{limit})");
        assert_eq!(page.total_count, 10);

        let proof = db
            .prove_bulk_position_range(
                vec![b"root".to_vec()],
                b"docs",
                start,
                limit,
                None,
                grove_version,
            )
            .unwrap()
            .expect("prove the range");
        let (proved_root, proved_page) = GroveDb::verify_bulk_position_range_proof(
            &proof,
            vec![b"root".to_vec()],
            b"docs",
            start,
            limit,
            grove_version,
        )
        .expect("verify the range");
        assert_eq!(proved_root, root, "proved [{start}, +{limit})");
        assert_eq!(proved_page.entries, expected, "proved [{start}, +{limit})");
        assert_eq!(proved_page.total_count, 10);
    }
}

#[test]
fn test_private_document_store_range_of_an_empty_store() {
    let grove_version = GroveVersion::latest();
    let db = make_db_with_store();
    let root = db.root_hash(None, grove_version).unwrap().unwrap();

    let page = db
        .private_document_store_get_range(&[b"root"], b"docs", 0, 10, None, grove_version)
        .unwrap()
        .expect("read an empty store");
    assert!(page.entries.is_empty());
    assert_eq!(page.total_count, 0);

    let proof = db
        .prove_bulk_position_range(vec![b"root".to_vec()], b"docs", 0, 10, None, grove_version)
        .unwrap()
        .expect("prove an empty store's page");
    let (proved_root, proved_page) = GroveDb::verify_bulk_position_range_proof(
        &proof,
        vec![b"root".to_vec()],
        b"docs",
        0,
        10,
        grove_version,
    )
    .expect("verify an empty store's page");
    assert_eq!(proved_root, root);
    assert!(proved_page.entries.is_empty());
    assert_eq!(proved_page.total_count, 0);
}

/// Re-encodes a range proof of `[root]/key` with its `BulkAppendTree` layer
/// dressed as a `CommitmentTree` layer: a made-up Sinsemilla root before the
/// same bulk proof.
fn disguised_as_a_commitment_tree_layer(proof: &[u8], key: &[u8]) -> Vec<u8> {
    let config = bincode::config::standard()
        .with_big_endian()
        .with_limit::<{ 256 * 1024 * 1024 }>();
    let (mut envelope, _): (GroveDBProof, usize) =
        bincode::decode_from_slice(proof, config).expect("decode the envelope");
    let GroveDBProof::V1(GroveDBProofV1 { root_layer }) = &mut envelope else {
        panic!("a range proof uses the V1 envelope");
    };
    let layer = root_layer
        .lower_layers
        .get_mut(b"root".as_slice())
        .and_then(|root| root.lower_layers.get_mut(key))
        .expect("the tree's layer");
    let ProofBytes::BulkAppendTree(bulk_bytes) = &layer.merk_proof else {
        panic!("the layer is a bulk append tree's proof");
    };
    let mut disguised = vec![0u8; 32];
    disguised.extend_from_slice(bulk_bytes);
    layer.merk_proof = ProofBytes::CommitmentTree(disguised);
    bincode::encode_to_vec(
        envelope,
        bincode::config::standard()
            .with_big_endian()
            .with_no_limit(),
    )
    .expect("re-encode the envelope")
}

/// A bulk proof dressed as a commitment tree's layer is refused for the
/// element it sits under, not merely for the root it derives: under a store,
/// which takes only its bulk proof, and under a `BulkAppendTree`, which is not
/// a commitment tree.
#[test]
fn test_private_document_store_layer_as_a_commitment_tree_layer_is_refused() {
    let grove_version = GroveVersion::latest();
    let db = make_db_with_entries(10);
    db.insert(
        &[b"root"],
        b"bulk",
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid config"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert the bulk append tree");
    for i in 0..6u8 {
        db.bulk_append(&[b"root"], b"bulk", entry(i + 1), None, grove_version)
            .unwrap()
            .expect("append to the bulk append tree");
    }

    for (key, refusal) in [
        (b"docs".as_slice(), "ProofBytes::BulkAppendTree"),
        (b"bulk".as_slice(), "non-CommitmentTree element"),
    ] {
        let proof = db
            .prove_bulk_position_range(vec![b"root".to_vec()], key, 0, 6, None, grove_version)
            .unwrap()
            .expect("prove the range");
        let result = GroveDb::verify_bulk_position_range_proof(
            &disguised_as_a_commitment_tree_layer(&proof, key),
            vec![b"root".to_vec()],
            key,
            0,
            6,
            grove_version,
        );
        assert!(
            matches!(
                result,
                Err(Error::InvalidProof(_, ref message)) if message.contains(refusal)
            ),
            "{}: got {:?}",
            String::from_utf8_lossy(key),
            result
        );
    }
}

/// A store's range proof charges the work of its bulk proof: the completed
/// chunk blobs it reads, the hash validating each one read from storage, and
/// the buffer's dense proof, which reads the buffer's stored node hashes. A `BulkAppendTree` holding the same
/// entries, proved the way it always was, charges none of it, so the store's
/// proof costs at least that much more.
#[test]
fn test_private_document_store_range_proof_charges_its_reads_and_hashes() {
    const ENTRY_SIZE: u32 = 1024;
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
        Element::empty_private_document_store(ENTRY_SIZE, TEST_CHUNK_POWER).expect("valid config"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert the store");
    db.insert(
        &[b"root"],
        b"bulk",
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid config"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert the bulk append tree");
    // 10 entries, 4 to a chunk: two compacted chunks and two buffered entries
    for i in 0..10u8 {
        let value = vec![i; ENTRY_SIZE as usize];
        db.private_document_store_insert(&[b"root"], b"docs", value.clone(), None, grove_version)
            .unwrap()
            .expect("append to the store");
        db.bulk_append(&[b"root"], b"bulk", value, None, grove_version)
            .unwrap()
            .expect("append to the bulk append tree");
    }

    let prove = |key: &[u8]| {
        let CostContext { value, cost } =
            db.prove_bulk_position_range(vec![b"root".to_vec()], key, 0, 10, None, grove_version);
        value.expect("prove the range");
        cost
    };
    let store = prove(b"docs");
    let bulk = prove(b"bulk");

    // The two chunk blobs alone hold 8 entries
    assert!(
        store.storage_loaded_bytes >= bulk.storage_loaded_bytes + 8 * u64::from(ENTRY_SIZE),
        "the chunk blobs read are charged: store {store:?}, bulk {bulk:?}"
    );
    assert!(
        store.seek_count > bulk.seek_count,
        "the reads are charged: store {store:?}, bulk {bulk:?}"
    );
    // Each of the two chunks read from storage is validated by its leaf hash
    assert!(
        store.hash_node_calls >= bulk.hash_node_calls + 2,
        "the chunks' validation is charged: store {store:?}, bulk {bulk:?}"
    );

    // The unproved range reads differ the same way
    let store_read = db
        .private_document_store_get_range(&[b"root"], b"docs", 0, 10, None, grove_version)
        .cost;
    let bulk_read = db
        .bulk_get_range(&[b"root"], b"bulk", 0, 10, None, grove_version)
        .cost;
    assert!(
        store_read.hash_node_calls >= bulk_read.hash_node_calls + 2,
        "the chunks' validation is charged on read: store {store_read:?}, bulk {bulk_read:?}"
    );
}

/// A store's range proof also charges bagging the MMR peaks right of the
/// proved chunks: 28 entries, 4 to a chunk, are seven chunks under peaks of
/// 4 + 2 + 1 chunks, and proving the first chunk bags the two right-hand
/// peaks, one merge the `BulkAppendTree` route does not charge.
#[test]
fn test_private_document_store_range_proof_charges_the_mmr_peak_bagging() {
    let grove_version = GroveVersion::latest();
    let db = make_db_with_entries(28);
    db.insert(
        &[b"root"],
        b"bulk",
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid config"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert the bulk append tree");
    for i in 0..28u8 {
        db.bulk_append(&[b"root"], b"bulk", entry(i + 1), None, grove_version)
            .unwrap()
            .expect("append to the bulk append tree");
    }

    let prove = |key: &[u8]| {
        let CostContext { value, cost } =
            db.prove_bulk_position_range(vec![b"root".to_vec()], key, 0, 4, None, grove_version);
        value.expect("prove the first chunk");
        cost
    };
    let store = prove(b"docs");
    let bulk = prove(b"bulk");
    assert!(
        store.hash_node_calls > bulk.hash_node_calls,
        "the peak bagging is charged: store {store:?}, bulk {bulk:?}"
    );
}

/// A store's lower layer must be its bulk append tree's proof, and the store's
/// range proofs must be active, whatever envelope the layer arrives in: a
/// Merk-tagged layer committing the store's true root, verified for a subset
/// query stopping at the store, is refused where the slot is 0, 2 or 1.
#[test]
fn test_private_document_store_lower_layer_is_gated_before_dispatch() {
    use grovedb_merk::proofs::{encoding::encode_into, Node, Op};

    let grove_version = GroveVersion::latest();
    let db = make_db_with_entries(10);
    let proof = db
        .prove_bulk_position_range(vec![b"root".to_vec()], b"docs", 0, 10, None, grove_version)
        .unwrap()
        .expect("prove the range");

    let config = bincode::config::standard()
        .with_big_endian()
        .with_limit::<{ 256 * 1024 * 1024 }>();
    let (mut envelope, _): (GroveDBProof, usize) =
        bincode::decode_from_slice(&proof, config).expect("decode the envelope");
    let GroveDBProof::V1(GroveDBProofV1 { root_layer }) = &mut envelope else {
        panic!("a range proof uses the V1 envelope");
    };
    let store_layer = root_layer
        .lower_layers
        .get_mut(b"root".as_slice())
        .and_then(|root| root.lower_layers.get_mut(b"docs".as_slice()))
        .expect("the store's layer");
    let ProofBytes::BulkAppendTree(bulk_bytes) = &store_layer.merk_proof else {
        panic!("a store's layer is its bulk append tree's proof");
    };
    // The store's true root, from the honest bulk proof
    let (bulk_root, _) =
        grovedb_bulk_append_tree::BulkAppendTreeProof::decode_from_slice(bulk_bytes)
            .expect("decode the bulk proof")
            .verify_and_compute_root(TEST_CHUNK_POWER, 10)
            .expect("verify the bulk proof");
    let store_root = grovedb_private_document_store::compute_private_document_store_state_root(
        &grovedb_private_document_store::private_document_store_config_hash(
            TEST_ENTRY_SIZE,
            TEST_CHUNK_POWER,
        ),
        &bulk_root,
    );
    let mut merk_bytes = Vec::new();
    encode_into([Op::Push(Node::Hash(store_root))].iter(), &mut merk_bytes);
    store_layer.merk_proof = ProofBytes::Merk(merk_bytes);
    store_layer.lower_layers.clear();
    let merk_tagged = bincode::encode_to_vec(
        envelope,
        bincode::config::standard()
            .with_big_endian()
            .with_no_limit(),
    )
    .expect("re-encode the envelope");

    let element_query = PathQuery::new_single_key(vec![b"root".to_vec()], b"docs".to_vec());
    for slot in [0, 2] {
        let mut version = GROVE_V5.clone();
        version
            .grovedb_versions
            .operations
            .private_document_store
            .range_proof = slot;
        let result = GroveDb::verify_subset_query(&merk_tagged, &element_query, &version);
        assert!(
            matches!(result, Err(Error::NotSupported(_))),
            "slot {slot}: got {:?}",
            result
        );
    }
    let result = GroveDb::verify_subset_query(&merk_tagged, &element_query, grove_version);
    assert!(
        matches!(
            result,
            Err(Error::InvalidProof(_, ref message)) if message.contains("ProofBytes::BulkAppendTree")
        ),
        "got {:?}",
        result
    );
}

/// Changing a proved entry's bytes either breaks the proof or moves the root
/// it derives off the database's.
#[test]
fn test_private_document_store_range_proof_refuses_a_changed_entry() {
    let grove_version = GroveVersion::latest();
    let db = make_db_with_entries(10);
    let root = db.root_hash(None, grove_version).unwrap().unwrap();

    // Entry 2 sits in the first completed chunk, entry 9 in the buffer.
    for position in [2u8, 9] {
        let proof = db
            .prove_bulk_position_range(vec![b"root".to_vec()], b"docs", 0, 10, None, grove_version)
            .unwrap()
            .expect("prove the range");
        let needle = entry(position + 1);
        let at = proof
            .windows(needle.len())
            .position(|window| window == needle.as_slice())
            .expect("the entry's bytes are in the proof");
        let mut tampered = proof.clone();
        tampered[at] ^= 0xFF;

        match GroveDb::verify_bulk_position_range_proof(
            &tampered,
            vec![b"root".to_vec()],
            b"docs",
            0,
            10,
            grove_version,
        ) {
            Err(_) => {}
            Ok((proved_root, _)) => assert_ne!(
                proved_root, root,
                "a changed entry {position} must not derive the database root"
            ),
        }
    }
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
    // The gate itself: all slots 0 on V3 (fail closed), 1 on V4 apart from
    // the range read and range proofs, which 6.1.1 shipped V4 without and
    // which arrive in V5.
    let v3 = &GROVE_V3.grovedb_versions.operations.private_document_store;
    assert_eq!(v3.element_creation, 0);
    assert_eq!(v3.insert, 0);
    assert_eq!(v3.get_value, 0);
    assert_eq!(v3.count, 0);
    assert_eq!(v3.get_range, 0);
    assert_eq!(v3.range_proof, 0);
    let v4 = &GROVE_V4.grovedb_versions.operations.private_document_store;
    assert_eq!(v4.element_creation, 1);
    assert_eq!(v4.insert, 1);
    assert_eq!(v4.get_value, 1);
    assert_eq!(v4.count, 1);
    assert_eq!(v4.get_range, 0);
    assert_eq!(v4.range_proof, 0);
    let v5 = &GROVE_V5.grovedb_versions.operations.private_document_store;
    assert_eq!(v5.get_range, 1);
    assert_eq!(v5.range_proof, 1);
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
    // Build the store under the latest version, then attempt V3 operations on it.
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

    let result = db
        .private_document_store_get_range(&[b"root"], b"docs", 0, 10, None, &GROVE_V3)
        .unwrap();
    assert!(
        matches!(result, Err(Error::VersionError(_))),
        "V3 get_range op must fail closed, got {:?}",
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
/// v2), so drive it with a custom version: V4 with the insert router and
/// the insert implementation slots both dialed back to 0. The v1 router
/// only composes with the v2 implementation (its cached-Merk body), so it
/// is dialed back together rather than silently falling back.
#[test]
fn test_private_document_store_insert_v0_element_path() {
    let mut custom = GROVE_V4.clone();
    custom
        .grovedb_versions
        .operations
        .insert
        .add_element_on_transaction = 0;
    custom
        .grovedb_versions
        .operations
        .insert
        .insert_on_transaction = 0;
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
fn test_private_document_store_element_display_and_type_str() {
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
                read_mode: None,
                limit: None,
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

// ===========================================================================
// Regression: wrapper preservation, write-once enforcement, guard exactness
// ===========================================================================

/// Read a `CountTree`'s recorded aggregate count.
fn count_tree_value(db: &crate::tests::TempGroveDb, key: &[u8]) -> u64 {
    let grove_version = GroveVersion::latest();
    match db
        .get(EMPTY_PATH, key, None, grove_version)
        .unwrap()
        .expect("get count tree")
    {
        Element::CountTree(_, count, _) => count,
        other => panic!("expected CountTree, got {}", other.type_str()),
    }
}

/// Build a `CountTree` at `counted` holding a `NonCounted`-wrapped store at
/// `counted/docs`, and return the parent's count before any append.
fn make_non_counted_store_under_count_tree() -> (crate::tests::TempGroveDb, u64) {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();
    db.insert(
        EMPTY_PATH,
        b"counted",
        Element::empty_count_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert count tree");

    let wrapped = Element::new_non_counted(
        Element::empty_private_document_store(TEST_ENTRY_SIZE, TEST_CHUNK_POWER)
            .expect("valid config"),
    )
    .expect("wrap in NonCounted");
    db.insert(&[b"counted"], b"docs", wrapped, None, None, grove_version)
        .unwrap()
        .expect("insert NonCounted store");

    let count = count_tree_value(&db, b"counted");
    assert_eq!(count, 0, "a NonCounted child must not be counted");
    (db, count)
}

#[test]
fn test_direct_append_preserves_non_counted_wrapper() {
    let grove_version = GroveVersion::latest();
    let (db, count_before) = make_non_counted_store_under_count_tree();

    db.private_document_store_insert(&[b"counted"], b"docs", entry(1), None, grove_version)
        .unwrap()
        .expect("append");

    // `get` resolves through wrappers by design, so read the RAW stored
    // element to see whether the wrapper survived.
    let stored = db
        .get_raw(
            [b"counted".as_slice()].as_ref().into(),
            b"docs",
            None,
            grove_version,
        )
        .unwrap()
        .expect("get raw store");
    assert!(
        stored.is_non_counted(),
        "a direct append must not strip the NonCounted wrapper"
    );
    assert!(stored.is_private_document_store());
    assert_eq!(
        count_tree_value(&db, b"counted"),
        count_before,
        "appending to a NonCounted store must not change the parent's count"
    );

    let issues = db
        .verify_grovedb(None, true, false, grove_version)
        .expect("verify_grovedb");
    assert!(issues.is_empty(), "issues: {:?}", issues);
}

#[test]
fn test_batch_append_preserves_non_counted_wrapper() {
    let grove_version = GroveVersion::latest();
    let (db, count_before) = make_non_counted_store_under_count_tree();

    // Several entries so the batch path goes through append_many and the
    // ReplaceNonMerkTreeRoot rebuild.
    let ops = (0..6u8)
        .map(|i| {
            QualifiedGroveDbOp::private_document_store_insert_op(
                vec![b"counted".to_vec(), b"docs".to_vec()],
                entry(i),
            )
        })
        .collect();
    db.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("batch append");

    let stored = db
        .get_raw(
            [b"counted".as_slice()].as_ref().into(),
            b"docs",
            None,
            grove_version,
        )
        .unwrap()
        .expect("get raw store");
    assert!(
        stored.is_non_counted(),
        "a batch append must not strip the NonCounted wrapper"
    );
    assert_eq!(stored.non_merk_entry_count(), Some(6));
    assert_eq!(
        count_tree_value(&db, b"counted"),
        count_before,
        "batch appending to a NonCounted store must not change the parent's count"
    );

    let issues = db
        .verify_grovedb(None, true, false, grove_version)
        .expect("verify_grovedb");
    assert!(issues.is_empty(), "issues: {:?}", issues);
}

#[test]
fn test_private_document_store_is_write_once() {
    let grove_version = GroveVersion::latest();
    let db = make_db_with_store();
    for i in 0..5u8 {
        db.private_document_store_insert(&[b"root"], b"docs", entry(i), None, grove_version)
            .unwrap()
            .expect("append");
    }
    let root_before = db.root_hash(None, grove_version).unwrap().unwrap();

    // Direct overwrite with a fresh empty store must be rejected, not
    // silently accepted (which would orphan the existing chunk data).
    let result = db
        .insert(
            &[b"root"],
            b"docs",
            Element::empty_private_document_store(TEST_ENTRY_SIZE, TEST_CHUNK_POWER)
                .expect("valid config"),
            None,
            None,
            grove_version,
        )
        .unwrap();
    assert!(
        matches!(
            result,
            Err(Error::InvalidInput(_)) | Err(Error::OverrideNotAllowed(_))
        ),
        "direct overwrite must be rejected, got {:?}",
        result
    );

    // Batch overwrite likewise.
    let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
        vec![b"root".to_vec()],
        b"docs".to_vec(),
        Element::empty_private_document_store(TEST_ENTRY_SIZE, TEST_CHUNK_POWER)
            .expect("valid config"),
    )];
    let result = db.apply_batch(ops, None, None, grove_version).unwrap();
    assert!(
        matches!(result, Err(Error::InvalidBatchOperation(_))),
        "batch overwrite must be rejected, got {:?}",
        result
    );

    // The store is untouched by the rejected overwrites.
    assert_eq!(
        db.private_document_store_count(&[b"root"], b"docs", None, grove_version)
            .unwrap()
            .expect("count"),
        5
    );
    assert_eq!(
        root_before,
        db.root_hash(None, grove_version).unwrap().unwrap(),
        "rejected overwrites must not change the root hash"
    );
}

#[test]
fn test_version_guard_rejects_unknown_future_slot() {
    // The guard is an exact match: a future slot value must NOT silently run
    // v1 semantics, or an un-upgraded node diverges from an upgraded one.
    let mut future = GROVE_V4.clone();
    future
        .grovedb_versions
        .operations
        .private_document_store
        .insert = 2;
    let db = make_db_with_store();

    let result = db
        .private_document_store_insert(&[b"root"], b"docs", entry(0), None, &future)
        .unwrap();
    assert!(
        matches!(result, Err(Error::VersionError(_))),
        "an unknown future slot must fail closed, got {:?}",
        result
    );

    let ops = vec![QualifiedGroveDbOp::private_document_store_insert_op(
        vec![b"root".to_vec(), b"docs".to_vec()],
        entry(0),
    )];
    let result = db.apply_batch(ops, None, None, &future).unwrap();
    assert!(
        matches!(result, Err(Error::VersionError(_))),
        "an unknown future slot must fail closed in batch too, got {:?}",
        result
    );
}

#[test]
fn test_private_document_store_reads_are_billed() {
    // An in-range read must charge for actually fetching the document, not
    // just for reading the parent element.
    let grove_version = GroveVersion::latest();
    let db = make_db_with_store();
    // Enough entries to span a completed chunk and the live buffer.
    for i in 0..6u8 {
        db.private_document_store_insert(&[b"root"], b"docs", entry(i), None, grove_version)
            .unwrap()
            .expect("append");
    }

    let out_of_range = db
        .private_document_store_get_value(&[b"root"], b"docs", 99, None, grove_version)
        .cost;
    // Position 0 lives in a completed chunk (MMR read), position 5 in the
    // buffer (dense-tree read); both must cost more than the element lookup.
    for pos in [0u64, 5] {
        let cost = db
            .private_document_store_get_value(&[b"root"], b"docs", pos, None, grove_version)
            .cost;
        assert!(
            cost.storage_loaded_bytes > out_of_range.storage_loaded_bytes,
            "reading position {} must load the document bytes (got {:?} vs out-of-range {:?})",
            pos,
            cost.storage_loaded_bytes,
            out_of_range.storage_loaded_bytes
        );
    }
}

#[test]
fn test_private_document_store_delete_empty_and_via_batch() {
    let grove_version = GroveVersion::latest();

    // Deleting an EMPTY store takes the is_empty branch, which the populated
    // delete test never reaches.
    let db = make_db_with_store();
    db.delete(&[b"root"], b"docs", None, None, grove_version)
        .unwrap()
        .expect("delete empty store");
    assert!(matches!(
        db.get(&[b"root"], b"docs", None, grove_version).unwrap(),
        Err(Error::PathKeyNotFound(_))
    ));
    let issues = db
        .verify_grovedb(None, true, false, grove_version)
        .expect("verify_grovedb after empty-store delete");
    assert!(issues.is_empty(), "issues: {:?}", issues);

    // Batch DeleteTree is a wholly different path (scan_delete_tree_ops /
    // non_merk_delete_paths) from the direct delete.
    let db = make_db_with_store();
    for i in 0..6u8 {
        db.private_document_store_insert(&[b"root"], b"docs", entry(i), None, grove_version)
            .unwrap()
            .expect("append");
    }
    let ops = vec![QualifiedGroveDbOp::delete_tree_op(
        vec![b"root".to_vec()],
        b"docs".to_vec(),
        grovedb_merk::tree_type::TreeType::PrivateDocumentStore(TEST_CHUNK_POWER),
        crate::batch::SubelementsDeletionBehavior::DeleteChildren,
    )];
    db.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("batch delete populated store");
    assert!(matches!(
        db.get(&[b"root"], b"docs", None, grove_version).unwrap(),
        Err(Error::PathKeyNotFound(_))
    ));

    let issues = db
        .verify_grovedb(None, true, false, grove_version)
        .expect("verify_grovedb");
    assert!(issues.is_empty(), "issues: {:?}", issues);
}

#[test]
fn test_private_document_store_reopened_reads_are_billed() {
    // A store reopened from persisted state resolves its MMR root through
    // the lazy (uncached) path — the path proof binding and the integrity
    // walk take. Against real storage that read must be billed.
    let grove_version = GroveVersion::latest();
    let db = make_db_with_store();
    for i in 0..6u8 {
        db.private_document_store_insert(&[b"root"], b"docs", entry(i), None, grove_version)
            .unwrap()
            .expect("append");
    }

    // A terminal proof over the store derives its state root from a freshly
    // opened (cache-cold) store.
    let path_query = PathQuery::new_unsized(
        vec![b"root".to_vec()],
        Query::new_single_key(b"docs".to_vec()),
    );
    let ctx = db.prove_query(&path_query, None, grove_version);
    let cost = ctx.cost.clone();
    ctx.value.expect("prove terminal store element");
    assert!(
        cost.storage_loaded_bytes > 0 && cost.seek_count > 0,
        "deriving a reopened store's state root during proof binding must bill \
         its storage reads, got {:?}",
        cost
    );
}

/// Creating a store derives its empty state root, which is two blake3 calls:
/// the committed-config hash and the composite `pds_state` hash. Neither is
/// visible to `insert_subtree`, which receives an already-computed array, so
/// both have to be charged at the creation site or the operation is billed two
/// hashes short.
///
/// Asserted as a difference against a `BulkAppendTree` creation rather than as
/// an absolute figure: that type creates from `NULL_HASH` and so performs
/// neither hash, while every other part of the insert — the parent Merk write,
/// the element value hash — is the same work. The gap is therefore exactly the
/// two hashes under test, and the test does not break when unrelated Merk
/// costs shift.
#[test]
fn test_private_document_store_creation_bills_its_two_hashes() {
    let grove_version = GroveVersion::latest();

    let pds_cost = {
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
            b"store",
            Element::empty_private_document_store(TEST_ENTRY_SIZE, TEST_CHUNK_POWER)
                .expect("valid config"),
            None,
            None,
            grove_version,
        )
        .cost
    };

    let bulk_cost = {
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
            b"store",
            Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid config"),
            None,
            None,
            grove_version,
        )
        .cost
    };

    assert_eq!(
        pds_cost.hash_node_calls,
        bulk_cost.hash_node_calls + 2,
        "store creation must bill the config hash and the composite root \
         hash (pds {:?} vs bulk {:?})",
        pds_cost,
        bulk_cost
    );
}

/// A store element that CLAIMS entries it has no data for must be refused at
/// creation, on every path.
///
/// `Element::PrivateDocumentStore` is a public variant with public fields, so
/// `total_count` is not guaranteed to come from
/// `Element::empty_private_document_store` — a caller can construct one
/// directly, and one can arrive by deserialization. Accepting it would commit
/// an element whose committed count has no backing chunks or buffer entries:
/// the state root would be derived as if empty, and reads of the claimed
/// positions would fail against a tree that verifies as intact.
#[test]
fn test_private_document_store_rejects_nonempty_total_count_at_creation() {
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

    let claims_five = Element::PrivateDocumentStore(5, TEST_ENTRY_SIZE, TEST_CHUNK_POWER, None);

    // Direct insert.
    let direct = db
        .insert(
            &[b"root"],
            b"store",
            claims_five.clone(),
            None,
            None,
            grove_version,
        )
        .unwrap();
    assert!(
        direct.is_err(),
        "direct insert must refuse a store claiming entries it has no data \
         for, got {:?}",
        direct
    );

    // Batch insert.
    let batch = vec![QualifiedGroveDbOp::insert_or_replace_op(
        vec![b"root".to_vec()],
        b"store".to_vec(),
        claims_five,
    )];
    let batched = db.apply_batch(batch, None, None, grove_version).unwrap();
    assert!(
        batched.is_err(),
        "batch insert must refuse it too, got {:?}",
        batched
    );

    // Nothing was created either way.
    assert!(
        db.get_raw(
            [b"root".as_slice()].as_ref().into(),
            b"store",
            None,
            grove_version
        )
        .unwrap()
        .is_err(),
        "no element should exist at the key"
    );
}
