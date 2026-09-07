//! Issue #893 — clearing a non-Merk data tree (MmrTree, BulkAppendTree,
//! DenseAppendOnlyFixedSizeTree, CommitmentTree, PrivateDocumentStore) must
//! leave the retained parent element and commitment describing the canonical
//! empty child state.
//!
//! Under `GROVE_V4`+ (`clear_subtree` v1) the payload clear, the parent
//! element reset (count zeroed, configuration / flags / wrapper retained)
//! and the upward propagation commit atomically, so a cleared tree is
//! indistinguishable — element, child hash and grovedb root — from a
//! freshly inserted empty tree of the same type, and a subsequent typed
//! append lands exactly where a first append into a fresh tree would.
//!
//! Under `GROVE_V1`..`GROVE_V3` (`clear_subtree` v0, live in production)
//! the incoherent legacy behaviour — payload gone, parent element and
//! commitment stale, nothing propagated — is pinned for replay
//! compatibility.

use grovedb_version::version::{v3::GROVE_V3, GroveVersion};

use crate::{
    tests::{common::EMPTY_PATH, make_empty_grovedb, TempGroveDb},
    Element,
};

/// A distinctive flags payload used to check flag retention.
const FLAGS: [u8; 3] = [7, 8, 9];

fn root_hash(db: &TempGroveDb, grove_version: &GroveVersion) -> [u8; 32] {
    db.root_hash(None, grove_version)
        .unwrap()
        .expect("root hash")
}

fn commitment_tree_row_insert(db: &TempGroveDb, key: &[u8], i: u8, grove_version: &GroveVersion) {
    let mut cmx = [0u8; 32];
    cmx[0] = i;
    cmx[31] &= 0x7f;
    let mut rho = [0u8; 32];
    rho[0] = i;
    rho[1] = 0xB0;
    let mut cv_net = [0u8; 32];
    cv_net[0] = i;
    cv_net[1] = 0xCC;
    let mut enc_data = [0u8; 104];
    enc_data[0] = i;
    let mut epk = [0u8; 32];
    epk[0] = i;
    let mut out_ct = [0u8; 80];
    out_ct[0] = i;
    let ciphertext = grovedb_commitment_tree::TransmittedNoteCiphertext::<
        grovedb_commitment_tree::DashMemo,
    >::from_parts(
        epk,
        grovedb_commitment_tree::NoteBytesData(enc_data),
        out_ct,
    );
    db.commitment_tree_insert(
        EMPTY_PATH,
        key,
        cmx,
        rho,
        cv_net,
        ciphertext,
        None,
        grove_version,
    )
    .unwrap()
    .expect("commitment tree insert");
}

/// Clear a populated tree and require its element, child commitment and the
/// grovedb root to be exactly those of a never-populated twin; then append
/// once into both and require they still agree.
fn assert_clear_matches_fresh_tree(
    element: Element,
    append: impl Fn(&TempGroveDb, u8),
    grove_version: &GroveVersion,
) {
    let key: &[u8] = b"tree";

    let cleared_db = make_empty_grovedb();
    cleared_db
        .insert(EMPTY_PATH, key, element.clone(), None, None, grove_version)
        .unwrap()
        .expect("insert tree");
    for i in 0..5u8 {
        append(&cleared_db, i);
    }

    let fresh_db = make_empty_grovedb();
    fresh_db
        .insert(EMPTY_PATH, key, element.clone(), None, None, grove_version)
        .unwrap()
        .expect("insert tree");

    assert_ne!(
        root_hash(&cleared_db, grove_version),
        root_hash(&fresh_db, grove_version),
        "appends must have moved the root before the clear"
    );

    let cleared = cleared_db
        .clear_subtree([key].as_ref(), None, None, grove_version)
        .expect("clear should succeed");
    assert!(cleared, "clear_subtree should return true");

    let stored = cleared_db
        .get_raw(EMPTY_PATH, key, None, grove_version)
        .unwrap()
        .expect("stored element after clear");
    assert_eq!(
        stored, element,
        "cleared element must equal the freshly inserted empty element \
         (count zeroed, configuration and flags retained)"
    );

    assert_eq!(
        root_hash(&cleared_db, grove_version),
        root_hash(&fresh_db, grove_version),
        "a cleared tree must commit the same root as a never-populated twin"
    );

    let issues = cleared_db
        .verify_grovedb(None, true, false, grove_version)
        .expect("verify should not fail");
    assert!(
        issues.is_empty(),
        "cleared database must be coherent, got: {:?}",
        issues
    );

    // A typed append after the clear must land exactly where a first append
    // into a fresh tree lands.
    append(&cleared_db, 42);
    append(&fresh_db, 42);
    assert_eq!(
        root_hash(&cleared_db, grove_version),
        root_hash(&fresh_db, grove_version),
        "an append after clear must match a first append into a fresh tree"
    );

    let issues = cleared_db
        .verify_grovedb(None, true, false, grove_version)
        .expect("verify should not fail");
    assert!(
        issues.is_empty(),
        "database must stay coherent after the post-clear append, got: {:?}",
        issues
    );
}

#[test]
fn clear_mmr_tree_resets_parent_element_and_commitment() {
    let grove_version = GroveVersion::latest();
    assert_clear_matches_fresh_tree(
        Element::empty_mmr_tree_with_flags(Some(FLAGS.to_vec())),
        |db, i| {
            db.mmr_tree_append(EMPTY_PATH, b"tree", vec![i], None, grove_version)
                .unwrap()
                .expect("append value");
        },
        grove_version,
    );

    // Typed reads after clear + re-append: one leaf, at index 0.
    let db = make_empty_grovedb();
    db.insert(
        EMPTY_PATH,
        b"tree",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert tree");
    for i in 0..5u8 {
        db.mmr_tree_append(EMPTY_PATH, b"tree", vec![i], None, grove_version)
            .unwrap()
            .expect("append value");
    }
    db.clear_subtree([b"tree".as_ref()].as_ref(), None, None, grove_version)
        .expect("clear should succeed");
    assert_eq!(
        db.mmr_tree_leaf_count(EMPTY_PATH, b"tree", None, grove_version)
            .unwrap()
            .expect("leaf count"),
        0
    );
    let (_, leaf_index) = db
        .mmr_tree_append(EMPTY_PATH, b"tree", vec![99], None, grove_version)
        .unwrap()
        .expect("append after clear");
    assert_eq!(leaf_index, 0, "append after clear must start over");
    assert_eq!(
        db.mmr_tree_get_value(EMPTY_PATH, b"tree", 0, None, grove_version)
            .unwrap()
            .expect("get value"),
        Some(vec![99])
    );
}

#[test]
fn clear_bulk_append_tree_resets_parent_element_and_commitment() {
    let grove_version = GroveVersion::latest();
    // Chunk power 2 → 4 rows per chunk, so 5 appends span a completed chunk
    // plus a partially filled buffer.
    let element = Element::empty_bulk_append_tree_with_flags(2, Some(FLAGS.to_vec()))
        .expect("valid chunk power");
    assert_clear_matches_fresh_tree(
        element,
        |db, i| {
            db.bulk_append(EMPTY_PATH, b"tree", vec![i, 0xB0], None, grove_version)
                .unwrap()
                .expect("bulk append");
        },
        grove_version,
    );
}

#[test]
fn clear_dense_tree_resets_parent_element_and_commitment() {
    let grove_version = GroveVersion::latest();
    assert_clear_matches_fresh_tree(
        Element::empty_dense_tree_with_flags(5, Some(FLAGS.to_vec())),
        |db, i| {
            db.dense_tree_insert(EMPTY_PATH, b"tree", vec![i, 0xD0], None, grove_version)
                .unwrap()
                .expect("dense insert");
        },
        grove_version,
    );
}

#[test]
fn clear_commitment_tree_resets_parent_element_and_commitment() {
    let grove_version = GroveVersion::latest();
    let element = Element::empty_commitment_tree_with_flags(2, Some(FLAGS.to_vec()))
        .expect("valid chunk power");
    assert_clear_matches_fresh_tree(
        element,
        |db, i| commitment_tree_row_insert(db, b"tree", i, grove_version),
        grove_version,
    );
}

#[test]
fn clear_private_document_store_resets_parent_element_and_commitment() {
    let grove_version = GroveVersion::latest();
    const ENTRY_SIZE: u32 = 48;
    let element = Element::empty_private_document_store(ENTRY_SIZE, 2).expect("valid config");
    assert_clear_matches_fresh_tree(
        element,
        |db, i| {
            db.private_document_store_insert(
                EMPTY_PATH,
                b"tree",
                vec![i; ENTRY_SIZE as usize],
                None,
                grove_version,
            )
            .unwrap()
            .expect("pds insert");
        },
        grove_version,
    );
}

/// A cleared tree nested below the root must propagate its reset commitment
/// through every ancestor.
#[test]
fn clear_nested_mmr_tree_propagates_to_root() {
    let grove_version = GroveVersion::latest();

    let build = |populate: bool| {
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
        db.insert(
            [b"parent".as_ref()].as_ref(),
            b"log",
            Element::empty_mmr_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert mmr tree");
        if populate {
            for i in 0..5u8 {
                db.mmr_tree_append(
                    [b"parent".as_ref()].as_ref(),
                    b"log",
                    vec![i],
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("append value");
            }
        }
        db
    };

    let cleared_db = build(true);
    let fresh_db = build(false);

    let cleared = cleared_db
        .clear_subtree(
            [b"parent".as_ref(), b"log".as_ref()].as_ref(),
            None,
            None,
            grove_version,
        )
        .expect("clear should succeed");
    assert!(cleared);

    assert_eq!(
        root_hash(&cleared_db, grove_version),
        root_hash(&fresh_db, grove_version),
        "the reset commitment must propagate through the parent to the root"
    );

    let issues = cleared_db
        .verify_grovedb(None, true, false, grove_version)
        .expect("verify should not fail");
    assert!(issues.is_empty(), "expected no issues, got: {:?}", issues);
}

/// A `NonCounted`-wrapped append tree keeps its wrapper (and the parent
/// count tree keeps its aggregate) across a clear.
///
/// Uses `PrivateDocumentStore` because it is the one non-Merk type whose
/// typed append preserves the wrapper: the MMR / bulk / dense / commitment
/// append paths drop `NonCounted` on rewrite — a known legacy defect kept
/// bug-for-bug because those types are live on `GROVE_V1`..`V3` (see the
/// `ReplaceNonMerkTreeRoot` note in `batch/mod.rs`) — so a wrapped tree of
/// those types no longer carries its wrapper by the time a clear runs.
#[test]
fn clear_non_counted_wrapped_private_document_store_preserves_wrapper() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    const ENTRY_SIZE: u32 = 48;

    db.insert(
        EMPTY_PATH,
        b"count",
        Element::empty_count_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert count tree");

    let mut inner = Element::empty_private_document_store(ENTRY_SIZE, 2).expect("valid config");
    inner.set_flags(Some(FLAGS.to_vec()));
    let wrapped = Element::new_non_counted(inner).expect("wrappable");
    db.insert(
        [b"count".as_ref()].as_ref(),
        b"store",
        wrapped.clone(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert wrapped private document store");

    for i in 0..3u8 {
        db.private_document_store_insert(
            [b"count".as_ref()].as_ref(),
            b"store",
            vec![i; ENTRY_SIZE as usize],
            None,
            grove_version,
        )
        .unwrap()
        .expect("pds insert");
    }

    let cleared = db
        .clear_subtree(
            [b"count".as_ref(), b"store".as_ref()].as_ref(),
            None,
            None,
            grove_version,
        )
        .expect("clear should succeed");
    assert!(cleared);

    let stored = db
        .get_raw(
            [b"count".as_ref()].as_ref().into(),
            b"store",
            None,
            grove_version,
        )
        .unwrap()
        .expect("stored element after clear");
    assert_eq!(
        stored, wrapped,
        "the NonCounted wrapper and flags must survive the clear"
    );

    let issues = db
        .verify_grovedb(None, true, false, grove_version)
        .expect("verify should not fail");
    assert!(issues.is_empty(), "expected no issues, got: {:?}", issues);
}

/// v0 pin: under `GROVE_V3` (live in production) the legacy incoherence is
/// preserved bug-for-bug — the payload is gone but the parent element keeps
/// the old mmr size and nothing propagates.
#[test]
fn clear_under_grove_v3_keeps_stale_parent_element() {
    let grove_version = &GROVE_V3;
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"tree",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert tree");
    for i in 0..5u8 {
        db.mmr_tree_append(EMPTY_PATH, b"tree", vec![i], None, grove_version)
            .unwrap()
            .expect("append value");
    }

    let root_before = root_hash(&db, grove_version);

    let cleared = db
        .clear_subtree([b"tree".as_ref()].as_ref(), None, None, grove_version)
        .expect("clear should succeed");
    assert!(cleared);

    assert_eq!(
        db.mmr_tree_leaf_count(EMPTY_PATH, b"tree", None, grove_version)
            .unwrap()
            .expect("leaf count"),
        5,
        "v0 must keep the stale count for replay compatibility"
    );
    assert_eq!(
        root_hash(&db, grove_version),
        root_before,
        "v0 must not propagate anything"
    );
}

// ---------------------------------------------------------------------------
// v0 ordinary-Merk coverage — the pre-`GROVE_V4` implementation is frozen in
// its own sub-file, so exercise its plain-Merk branches (identical in
// behaviour to v1) under `GROVE_V3` explicitly.
// ---------------------------------------------------------------------------

#[test]
fn clear_ordinary_tree_under_grove_v3_clears_and_propagates() {
    let grove_version = &GROVE_V3;
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"tree",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert tree");

    let empty_root = root_hash(&db, grove_version);

    for i in 0u8..5 {
        db.insert(
            [b"tree".as_ref()].as_ref(),
            &[i],
            Element::new_item(vec![i; 10]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item");
    }
    assert_ne!(root_hash(&db, grove_version), empty_root);

    let cleared = db
        .clear_subtree([b"tree".as_ref()].as_ref(), None, None, grove_version)
        .expect("clear should succeed");
    assert!(cleared);

    assert!(db
        .is_empty_tree([b"tree".as_ref()].as_ref(), None, grove_version)
        .unwrap()
        .expect("emptiness check"));
    assert_eq!(
        root_hash(&db, grove_version),
        empty_root,
        "an ordinary-Merk clear must propagate under v0 exactly as before"
    );
}

#[test]
fn clear_ordinary_tree_with_subtrees_under_grove_v3_option_branches() {
    use crate::operations::delete::ClearOptions;

    let grove_version = &GROVE_V3;
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"tree",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert tree");
    db.insert(
        [b"tree".as_ref()].as_ref(),
        b"inner",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert nested tree");

    // Default options: error out on the nested subtree.
    let result = db.clear_subtree([b"tree".as_ref()].as_ref(), None, None, grove_version);
    assert!(
        matches!(
            result,
            Err(crate::Error::ClearingTreeWithSubtreesNotAllowed(_))
        ),
        "expected ClearingTreeWithSubtreesNotAllowed, got {result:?}"
    );

    // Same, but asked to report instead of erroring.
    let cleared = db
        .clear_subtree(
            [b"tree".as_ref()].as_ref(),
            Some(ClearOptions {
                check_for_subtrees: true,
                allow_deleting_subtrees: false,
                trying_to_clear_with_subtrees_returns_error: false,
            }),
            None,
            grove_version,
        )
        .expect("clear should not error");
    assert!(!cleared, "clear must report false when subtrees remain");

    // Allowed to delete subtrees: clears everything.
    let cleared = db
        .clear_subtree(
            [b"tree".as_ref()].as_ref(),
            Some(ClearOptions {
                check_for_subtrees: true,
                allow_deleting_subtrees: true,
                trying_to_clear_with_subtrees_returns_error: false,
            }),
            None,
            grove_version,
        )
        .expect("clear should succeed");
    assert!(cleared);
    assert!(db
        .is_empty_tree([b"tree".as_ref()].as_ref(), None, grove_version)
        .unwrap()
        .expect("emptiness check"));
}

#[test]
fn clear_subtree_unknown_version_is_rejected() {
    use grovedb_version::version::v4::GROVE_V4;

    let db = make_empty_grovedb();
    db.insert(
        EMPTY_PATH,
        b"tree",
        Element::empty_tree(),
        None,
        None,
        GroveVersion::latest(),
    )
    .unwrap()
    .expect("insert tree");

    let mut unknown = GROVE_V4.clone();
    unknown.grovedb_versions.operations.delete.clear_subtree = 9;

    let result = db.clear_subtree([b"tree".as_ref()].as_ref(), None, None, &unknown);
    match result {
        Err(crate::Error::VersionError(
            grovedb_version::error::GroveVersionError::UnknownVersionMismatch {
                method,
                known_versions,
                received,
            },
        )) => {
            assert_eq!(method, "clear_subtree");
            assert_eq!(known_versions, vec![0, 1]);
            assert_eq!(received, 9);
        }
        other => panic!("expected UnknownVersionMismatch, got {other:?}"),
    }
}
