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

    // i64 SumTree control: the child goes in EMPTY and derives its sum from a
    // sum item written inside it. Asserting the sum on a rootless child is
    // rejected now — there would be nothing to derive it from.
    db.insert_into_provable_sum_indexed_tree(
        [b"psit".as_slice()].as_ref(),
        b"sum",
        Element::empty_sum_tree(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("i64 SumTree control");
    db.insert(
        [b"psit".as_slice(), b"sum".as_slice()].as_ref(),
        b"v",
        Element::new_sum_item(42),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("populate the control child so its sum derives to 42");
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

/// Overwriting an indexed tree with a bare `Reference` schedules the per-axis
/// secondary cleanup like every other overwrite-capable op (closes issue
/// https://github.com/dashpay/grovedb/issues/776).
///
/// This used to be deferred because routing reference overwrites through the
/// classifier started with a dedicated stored-element read (+1 seek, +79
/// storage_loaded_bytes on every reference overwrite — a live cost change).
/// The V4 gate now classifies from the old value the merk walk already
/// fetched, so including references costs nothing and the hole is closed on
/// V4+ while V1..V3 keep their released behaviour.
#[test]
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

/// The derived-count flow: insert children EMPTY, populate them, and let
/// propagation supply each child's ordering value.
///
/// This is the flow that replaces caller-asserted counts. It has to work for
/// "aggregates are always derived" to be a viable rule rather than a
/// restriction that makes the index unusable.
#[test]
fn derived_counts_order_the_secondary_index() {
    let grove_version = GroveVersion::latest();
    let db = crate::tests::make_test_grovedb(grove_version);

    db.insert(
        [crate::tests::TEST_LEAF].as_ref(),
        b"cidx",
        Element::empty_provable_count_indexed_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("create PCIT");

    // Three children, inserted EMPTY (count 0), then populated with 1, 3 and
    // 2 items respectively.
    for (child, n) in [
        (b"a".as_slice(), 1usize),
        (b"b".as_slice(), 3),
        (b"c".as_slice(), 2),
    ] {
        db.insert_into_count_indexed_tree(
            [crate::tests::TEST_LEAF, b"cidx"].as_ref(),
            child,
            Element::empty_provable_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert empty child");

        for i in 0..n {
            db.insert(
                [crate::tests::TEST_LEAF, b"cidx", child].as_ref(),
                &[b'i', i as u8],
                Element::new_item(vec![i as u8]),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("populate child");
        }
    }

    // Descending top-k must rank by the DERIVED counts: b(3), c(2), a(1).
    let top = db
        .indexed_count_top_k(
            [crate::tests::TEST_LEAF, b"cidx"].as_ref(),
            3,
            true,
            None,
            grove_version,
        )
        .unwrap()
        .expect("top_k");
    let order: Vec<Vec<u8>> = top.iter().map(|e| e.primary_key.clone()).collect();
    assert_eq!(
        order,
        vec![b"b".to_vec(), b"c".to_vec(), b"a".to_vec()],
        "secondary must be ordered by the counts propagation derived, got {order:?}"
    );

    assert!(
        db.verify_grovedb(None, true, true, grove_version)
            .expect("verify")
            .is_empty(),
        "derived-count state must verify clean with no indexed-primary exemption"
    );
}

/// The rootless-aggregate rule must hold on the BATCH path too, not just the
/// dedicated insert APIs.
///
/// An `insert_or_replace_op` carrying `ProvableCountTree(None, 9, None)` under
/// a PCIT used to be accepted and written; `verify_grovedb` then reported the
/// child as an aggregate mismatch — recorded count 9 against an empty inner
/// Merk. Same forgery as the dedicated path, different door.
#[test]
fn batch_rejects_rootless_aggregate_child_under_indexed_primary() {
    let grove_version = GroveVersion::latest();
    let db = crate::tests::make_test_grovedb(grove_version);

    db.insert(
        [crate::tests::TEST_LEAF].as_ref(),
        b"cidx",
        Element::empty_provable_count_indexed_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("create PCIT");

    let forged = vec![crate::batch::QualifiedGroveDbOp::insert_or_replace_op(
        vec![crate::tests::TEST_LEAF.to_vec(), b"cidx".to_vec()],
        b"b".to_vec(),
        Element::new_provable_count_tree_with_flags_and_count_value(None, 9, None),
    )];
    let result = db.apply_batch(forged, None, None, grove_version).unwrap();
    assert!(
        matches!(
            &result,
            Err(crate::Error::InvalidBatchOperation(m))
                if m.contains("non-zero aggregate while having no root key")
        ),
        "batch must refuse a rootless child claiming an aggregate, got {result:?}"
    );

    // The legitimate route: create the child empty and populate it in the same
    // batch, so the count is derived.
    let derived: Vec<_> = std::iter::once(crate::batch::QualifiedGroveDbOp::insert_or_replace_op(
        vec![crate::tests::TEST_LEAF.to_vec(), b"cidx".to_vec()],
        b"b".to_vec(),
        Element::empty_provable_count_tree(),
    ))
    .chain((0..9u8).map(|i| {
        crate::batch::QualifiedGroveDbOp::insert_or_replace_op(
            vec![
                crate::tests::TEST_LEAF.to_vec(),
                b"cidx".to_vec(),
                b"b".to_vec(),
            ],
            vec![i],
            Element::new_item(vec![i]),
        )
    }))
    .collect();
    db.apply_batch(derived, None, None, grove_version)
        .unwrap()
        .expect("derived-count batch must be accepted");

    let top = db
        .indexed_count_top_k(
            [crate::tests::TEST_LEAF, b"cidx"].as_ref(),
            1,
            true,
            None,
            grove_version,
        )
        .unwrap()
        .expect("top_k");
    assert_eq!(
        top,
        vec![(9, b"b".to_vec())],
        "the derived count must reach the secondary index"
    );
    assert!(
        db.verify_grovedb(None, true, true, grove_version)
            .expect("verify")
            .is_empty(),
        "derived-count batch state must verify clean"
    );
}

/// `GroveOp::Patch` must be subject to the rootless-aggregate rule too.
///
/// The batch guard originally matched only the insert/replace ops and fell
/// through `_ => continue` for everything else, so a Patch carrying
/// `ProvableCountTree(None, 9, None)` was written unchecked: it changed the
/// authenticated root and the forged 9 came back out of top-k.
#[test]
fn batch_patch_cannot_forge_a_rootless_aggregate() {
    let grove_version = GroveVersion::latest();
    let db = crate::tests::make_test_grovedb(grove_version);

    db.insert(
        [crate::tests::TEST_LEAF].as_ref(),
        b"cidx",
        Element::empty_provable_count_indexed_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("create PCIT");

    let root_before = db.root_hash(None, grove_version).unwrap().unwrap();

    let forged = vec![crate::batch::QualifiedGroveDbOp::patch_op(
        vec![crate::tests::TEST_LEAF.to_vec(), b"cidx".to_vec()],
        b"p".to_vec(),
        Element::new_provable_count_tree_with_flags_and_count_value(None, 9, None),
        0,
    )];
    let result = db.apply_batch(forged, None, None, grove_version).unwrap();
    assert!(
        matches!(
            &result,
            Err(crate::Error::InvalidBatchOperation(m))
                if m.contains("non-zero aggregate while having no root key")
        ),
        "Patch must be held to the same rule as insert/replace, got {result:?}"
    );
    assert_eq!(
        db.root_hash(None, grove_version).unwrap().unwrap(),
        root_before,
        "a rejected patch must not move the authenticated root"
    );
    assert!(
        db.indexed_count_top_k(
            [crate::tests::TEST_LEAF, b"cidx"].as_ref(),
            10,
            true,
            None,
            grove_version,
        )
        .unwrap()
        .expect("top_k")
        .is_empty(),
        "no forged entry may surface through the secondary index"
    );
}

/// The DeleteTree cleanup-type fix is gated on V4: active there, absent on the
/// released versions.
///
/// The check derives the stored type from data the apply already loads, so it
/// no longer costs anything — but it still flips an accepted/rejected
/// outcome: a mismatched declare that V1..V3 accept is refused on V4+ when an
/// indexed tree is involved. This pins both halves of the gate, so a future
/// change cannot quietly extend it to a released version (which would be a
/// consensus divergence) or drop it from V4 (which would reopen the
/// type-confusion).
#[test]
fn delete_tree_cleanup_type_gate_is_v4_only() {
    use grovedb_version::version::{v3::GROVE_V3, v4::GROVE_V4};

    // The slot itself: released versions read the declared type, V4 the stored.
    assert_eq!(
        GROVE_V3
            .grovedb_versions
            .apply_batch
            .delete_tree_cleanup_type_source,
        0,
        "V3 is live; it must keep taking the declared tree type at face value"
    );
    assert_eq!(
        GROVE_V4
            .grovedb_versions
            .apply_batch
            .delete_tree_cleanup_type_source,
        1,
        "V4 must select cleanup namespaces from the stored element's type"
    );

    // Behaviour: a mismatched declared type on a populated PCIT.
    let build = |gv: &GroveVersion| {
        let db = make_test_grovedb(gv);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("create PCIT");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"row",
            Element::new_item(b"value".to_vec()),
            None,
            gv,
        )
        .unwrap()
        .expect("populate PCIT");
        db
    };
    let mismatched_delete = || {
        vec![QualifiedGroveDbOp::delete_tree_op(
            vec![TEST_LEAF.to_vec()],
            b"cidx".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::DeleteChildren,
        )]
    };

    // V4: rejected, because the declared NormalTree hides a stored indexed
    // primary and would skip the per-axis secondary sweep.
    let v4_db = build(&GROVE_V4);
    let v4_result = v4_db
        .apply_batch(mismatched_delete(), None, None, &GROVE_V4)
        .unwrap();
    assert!(
        matches!(
            &v4_result,
            Err(crate::Error::InvalidBatchOperation(m))
                if m.contains("declared tree type does not match")
        ),
        "V4 must reject the mismatch, got {v4_result:?}"
    );

    // V3: accepted, exactly as it is today. This is the released behaviour the
    // gate exists to preserve — not an endorsement of it.
    let v3_db = build(&GROVE_V3);
    v3_db
        .apply_batch(mismatched_delete(), None, None, &GROVE_V3)
        .unwrap()
        .expect("V3 must keep accepting the mismatch; changing that is a consensus break");
}
