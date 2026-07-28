//! Cross-cutting security regressions for specialized indexed-tree insertion.

use grovedb_element::{indexed::IndexAxis, reference_path::ReferencePathType};
use grovedb_merk::tree_type::TreeType;
use grovedb_version::version::GroveVersion;
use tempfile::TempDir;

use crate::{
    batch::{QualifiedGroveDbOp, SubelementsDeletionBehavior},
    tests::{common::EMPTY_PATH, make_test_grovedb, TEST_LEAF},
    Element, Error, GroveDb,
};

fn assert_verify_passes(db: &GroveDb, grove_version: &GroveVersion) {
    let issues = db
        .verify_grovedb(None, true, true, grove_version)
        .expect("integrity traversal");
    assert!(issues.is_empty(), "integrity issues: {issues:?}");
}

#[test]
fn commitment_tree_under_pcit_uses_specialized_empty_root() {
    let grove_version = GroveVersion::latest();
    let tmp = TempDir::new().expect("temporary database");
    {
        let db = GroveDb::open(tmp.path()).expect("open database");
        db.insert(
            EMPTY_PATH,
            b"pcit",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PCIT");
        db.insert_into_count_indexed_tree(
            [b"pcit".as_slice()].as_ref(),
            b"commitment",
            Element::empty_commitment_tree(10).expect("valid CommitmentTree"),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert CommitmentTree");

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("integrity traversal");
        assert!(
            issues.is_empty(),
            "specialized child binding drifted: {issues:?}"
        );
    }
    GroveDb::open_with_cidx_integrity_check(tmp.path(), grove_version)
        .expect("checked reopen accepts specialized child binding");
}

#[test]
fn psit_rejects_big_sum_tree_at_i64_boundary() {
    let grove_version = GroveVersion::latest();
    let tmp = TempDir::new().expect("temporary database");
    let db = GroveDb::open(tmp.path()).expect("open database");
    db.insert(
        EMPTY_PATH,
        b"psit",
        Element::empty_provable_sum_indexed_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("create PSIT");

    let result = db
        .insert_into_provable_sum_indexed_tree(
            [b"psit".as_slice()].as_ref(),
            b"big",
            Element::new_big_sum_tree_with_flags_and_sum_value(None, 42, None),
            None,
            grove_version,
        )
        .unwrap();
    assert!(matches!(result, Err(crate::Error::InvalidInput(_))));

    db.insert_into_provable_sum_indexed_tree(
        [b"psit".as_slice()].as_ref(),
        b"sum",
        Element::new_sum_tree_with_flags_and_sum_value(None, 42, None),
        None,
        grove_version,
    )
    .unwrap()
    .expect("i64 SumTree control");
    assert_eq!(
        db.indexed_sum_top_k([b"psit".as_slice()].as_ref(), 10, true, None, grove_version,)
            .unwrap()
            .unwrap(),
        vec![(42, b"sum".to_vec())]
    );
}

#[test]
fn delete_tree_rejects_declared_type_mismatch() {
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
    .expect("create PCIT");
    db.insert_into_count_indexed_tree(
        [TEST_LEAF, b"cidx"].as_ref(),
        b"row",
        Element::new_item(b"value".to_vec()),
        None,
        grove_version,
    )
    .unwrap()
    .expect("populate PCIT");

    let result = db
        .apply_batch(
            vec![QualifiedGroveDbOp::delete_tree_op(
                vec![TEST_LEAF.to_vec()],
                b"cidx".to_vec(),
                TreeType::NormalTree,
                SubelementsDeletionBehavior::DeleteChildren,
            )],
            None,
            None,
            grove_version,
        )
        .unwrap();
    assert!(matches!(result, Err(Error::InvalidBatchOperation(_))));
    assert_eq!(
        db.indexed_count_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version,)
            .unwrap()
            .unwrap(),
        vec![(1, b"row".to_vec())]
    );
    assert_verify_passes(&db, grove_version);
}

#[test]
fn batch_overwrite_cleans_psit_and_pcpsit_namespaces() {
    let grove_version = GroveVersion::latest();

    let psit = make_test_grovedb(grove_version);
    psit.insert(
        [TEST_LEAF].as_ref(),
        b"indexed",
        Element::empty_provable_sum_indexed_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("create PSIT");
    psit.insert_into_provable_sum_indexed_tree(
        [TEST_LEAF, b"indexed"].as_ref(),
        b"stale",
        Element::new_sum_item(42),
        None,
        grove_version,
    )
    .unwrap()
    .expect("populate PSIT");
    for replacement in [
        Element::new_item(b"replacement".to_vec()),
        Element::empty_provable_sum_indexed_tree(),
    ] {
        psit.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"indexed".to_vec(),
                replacement,
            )],
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("overwrite PSIT");
    }
    assert!(psit
        .indexed_sum_top_k(
            [TEST_LEAF, b"indexed"].as_ref(),
            10,
            true,
            None,
            grove_version,
        )
        .unwrap()
        .unwrap()
        .is_empty());
    assert_verify_passes(&psit, grove_version);

    let pcpsit = make_test_grovedb(grove_version);
    let axes = vec![(IndexAxis::Count.tag(), None), (IndexAxis::Sum.tag(), None)];
    pcpsit
        .insert(
            [TEST_LEAF].as_ref(),
            b"indexed",
            Element::empty_provable_count_provable_sum_indexed_tree(axes.clone()).unwrap(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PCPSIT");
    pcpsit
        .insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"indexed"].as_ref(),
            b"stale",
            Element::new_item_with_sum_item(b"value".to_vec(), 42),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate PCPSIT");
    for replacement in [
        Element::new_item(b"replacement".to_vec()),
        Element::empty_provable_count_provable_sum_indexed_tree(axes).unwrap(),
    ] {
        pcpsit
            .apply_batch(
                vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"indexed".to_vec(),
                    replacement,
                )],
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("overwrite PCPSIT");
    }
    assert!(pcpsit
        .indexed_count_top_k(
            [TEST_LEAF, b"indexed"].as_ref(),
            10,
            true,
            None,
            grove_version,
        )
        .unwrap()
        .unwrap()
        .is_empty());
    assert!(pcpsit
        .indexed_sum_top_k(
            [TEST_LEAF, b"indexed"].as_ref(),
            10,
            true,
            None,
            grove_version,
        )
        .unwrap()
        .unwrap()
        .is_empty());
    assert_verify_passes(&pcpsit, grove_version);
}

/// DEFERRED — documents a known-open issue, not current behaviour.
///
/// Overwriting an indexed tree with a bare `Reference` does not schedule the
/// per-axis secondary cleanup, so the secondary namespace is orphaned. The fix
/// is to route reference overwrites through `inspect_cidx_overwrite` like every
/// other overwrite-capable op — but that costs one extra stored-element read on
/// EVERY reference overwrite (+1 seek, +79 storage_loaded_bytes, measured by
/// `single_insert_cost_tests::test_batch_root_one_update_item_*_with_refresh_reference`).
/// Cost feeds fees, and references over plain trees are shipped functionality on
/// GROVE_V1/V2/V3, so paying that read unconditionally changes live behaviour.
///
/// The hole requires an indexed tree to be the overwritten element, which cannot
/// happen on any released version — indexed trees are introduced by this PR. It
/// should therefore be closed together with the protocol version that activates
/// indexed trees, gated so live versions keep today's cost. Un-ignore this test
/// as part of that change.
#[test]
#[ignore = "deferred: closing this changes reference-overwrite cost on live versions; \
            gate with the protocol version that activates indexed trees"]
fn bare_reference_overwrite_cleans_indexed_storage() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    db.insert(
        [TEST_LEAF].as_ref(),
        b"target",
        Element::new_item(b"target".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("target");
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
        b"stale",
        Element::new_item(b"value".to_vec()),
        None,
        grove_version,
    )
    .unwrap()
    .expect("populate PCIT");

    db.apply_batch(
        vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"cidx".to_vec(),
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"target".to_vec(),
            ])),
        )],
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("bare reference overwrite");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"cidx",
        Element::empty_provable_count_indexed_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("recreate PCIT");
    assert!(db
        .indexed_count_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version,)
        .unwrap()
        .unwrap()
        .is_empty());
    assert_verify_passes(&db, grove_version);
}

#[test]
fn batch_count_changes_remove_all_old_secondary_rows_first() {
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
    .expect("create PCIT");
    for child in [b"a".as_slice(), b"b".as_slice()] {
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            child,
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("create child");
        db.insert(
            [TEST_LEAF, b"cidx", child].as_ref(),
            b"seed",
            Element::new_item(b"value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("seed child");
    }
    db.apply_batch(
        [b"a".as_slice(), b"b".as_slice()]
            .into_iter()
            .map(|child| {
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"cidx".to_vec(), child.to_vec()],
                    b"next".to_vec(),
                    Element::new_item(b"value".to_vec()),
                )
            })
            .collect(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("update both children");
    assert_eq!(
        db.indexed_count_top_k(
            [TEST_LEAF, b"cidx"].as_ref(),
            10,
            false,
            None,
            grove_version,
        )
        .unwrap()
        .unwrap(),
        vec![(2, b"a".to_vec()), (2, b"b".to_vec())]
    );
    assert_verify_passes(&db, grove_version);
}
