//! A batch applied with its consistency check disabled that inserts trees of
//! different tree types at one key.
//!
//! The child Merk is reserved with the first insert's tree type and a later
//! insert does not change it, so `GROVE_V1`..`V4` store a tree whose type is
//! not the one the last insert asked for, and for a sum tree followed by a
//! plain tree a state `verify_grovedb` reports. `GROVE_V5` refuses the batch
//! (`apply_batch.same_key_tree_type_conflict`).

#![cfg(feature = "minimal")]
#![allow(clippy::result_large_err)]

use grovedb::{
    batch::{BatchApplyOptions, QualifiedGroveDbOp},
    Element, Error, GroveDb,
};
use grovedb_version::version::{v4::GROVE_V4, v5::GROVE_V5, GroveVersion};
use tempfile::TempDir;

const PARENT: &[u8] = b"parent";
const KEY: &[u8] = b"key";
const CHILD: &[u8] = b"child";

/// A grove holding an empty tree at `[parent]`.
fn grove(grove_version: &GroveVersion) -> (TempDir, GroveDb) {
    let dir = TempDir::new().expect("expected a temporary directory");
    let db = GroveDb::open(dir.path()).expect("expected to open the grove");
    db.insert(
        [].as_ref() as &[&[u8]],
        PARENT,
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("expected to set up the grove");
    (dir, db)
}

fn at_key(element: Element) -> QualifiedGroveDbOp {
    QualifiedGroveDbOp::insert_or_replace_op(vec![PARENT.to_vec()], KEY.to_vec(), element)
}

fn under_key(element: Element) -> QualifiedGroveDbOp {
    QualifiedGroveDbOp::insert_or_replace_op(
        vec![PARENT.to_vec(), KEY.to_vec()],
        CHILD.to_vec(),
        element,
    )
}

fn apply_without_consistency_check(
    db: &GroveDb,
    ops: Vec<QualifiedGroveDbOp>,
    grove_version: &GroveVersion,
) -> Result<(), Error> {
    db.apply_batch(
        ops,
        Some(BatchApplyOptions {
            disable_operation_consistency_check: true,
            ..Default::default()
        }),
        None,
        grove_version,
    )
    .unwrap()
}

fn stored_at_key(db: &GroveDb, grove_version: &GroveVersion) -> Result<Element, Error> {
    db.get([PARENT].as_ref(), KEY, None, grove_version).unwrap()
}

fn issues(db: &GroveDb, grove_version: &GroveVersion) -> usize {
    db.visualize_verify_grovedb(None, true, false, grove_version)
        .expect("expected to verify the grove")
        .len()
}

fn assert_refused(db: &GroveDb, ops: Vec<QualifiedGroveDbOp>, grove_version: &GroveVersion) {
    let root_hash_before = db.root_hash(None, grove_version).unwrap().unwrap();
    let result = apply_without_consistency_check(db, ops, grove_version);
    assert!(
        matches!(
            result,
            Err(Error::InvalidBatchOperation(
                "batch inserts trees of different tree types at one key"
            ))
        ),
        "expected the batch to be refused, got {result:?}"
    );
    assert!(
        matches!(
            stored_at_key(db, grove_version),
            Err(Error::PathKeyNotFound(_))
        ),
        "expected nothing stored at the key"
    );
    assert_eq!(
        db.root_hash(None, grove_version).unwrap().unwrap(),
        root_hash_before
    );
    assert_eq!(issues(db, grove_version), 0);
}

#[test]
fn should_refuse_a_tree_then_a_sum_tree_at_one_key_on_v5() {
    let grove_version = &GROVE_V5;
    let (_dir, db) = grove(grove_version);
    assert_refused(
        &db,
        vec![
            at_key(Element::empty_tree()),
            at_key(Element::empty_sum_tree()),
            under_key(Element::new_sum_item(7)),
        ],
        grove_version,
    );
}

#[test]
fn should_refuse_a_sum_tree_then_a_tree_at_one_key_on_v5() {
    let grove_version = &GROVE_V5;
    let (_dir, db) = grove(grove_version);
    assert_refused(
        &db,
        vec![
            at_key(Element::empty_sum_tree()),
            at_key(Element::empty_tree()),
            under_key(Element::new_sum_item(7)),
        ],
        grove_version,
    );
}

#[test]
fn should_refuse_two_tree_types_at_one_key_with_nothing_under_it_on_v5() {
    let grove_version = &GROVE_V5;
    let (_dir, db) = grove(grove_version);
    assert_refused(
        &db,
        vec![
            at_key(Element::empty_tree()),
            at_key(Element::empty_count_tree()),
        ],
        grove_version,
    );
}

#[test]
fn should_refuse_two_tree_types_at_one_key_with_an_item_between_them_on_v5() {
    // The item replaces the first tree in the batch's operations, but the
    // Merk that tree reserved is still there for the sum tree to reuse.
    let grove_version = &GROVE_V5;
    let (_dir, db) = grove(grove_version);
    assert_refused(
        &db,
        vec![
            at_key(Element::empty_tree()),
            at_key(Element::new_item(vec![1])),
            at_key(Element::empty_sum_tree()),
            under_key(Element::new_sum_item(7)),
        ],
        grove_version,
    );
}

#[test]
fn should_refuse_a_merk_tree_then_a_non_merk_tree_at_one_key_on_v5() {
    let grove_version = &GROVE_V5;
    let (_dir, db) = grove(grove_version);
    assert_refused(
        &db,
        vec![
            at_key(Element::empty_tree()),
            at_key(Element::empty_mmr_tree()),
        ],
        grove_version,
    );
}

#[test]
fn should_apply_two_inserts_of_one_tree_type_at_one_key_on_every_version() {
    for grove_version in [&GROVE_V4, &GROVE_V5] {
        let (_dir, db) = grove(grove_version);
        apply_without_consistency_check(
            &db,
            vec![
                at_key(Element::empty_sum_tree_with_flags(Some(vec![1]))),
                at_key(Element::empty_sum_tree_with_flags(Some(vec![2]))),
                under_key(Element::new_sum_item(7)),
            ],
            grove_version,
        )
        .expect("expected the batch to apply");
        // The last insert's flags are stored, over the child the batch wrote.
        assert_eq!(
            stored_at_key(&db, grove_version).expect("expected the tree"),
            Element::new_sum_tree_with_flags_and_sum_value(Some(CHILD.to_vec()), 7, Some(vec![2])),
            "v{}",
            grove_version.protocol_version
        );
        assert_eq!(issues(&db, grove_version), 0);
    }
}

#[test]
fn should_refuse_two_tree_types_at_one_key_with_the_consistency_check_on_every_version() {
    for grove_version in [&GROVE_V4, &GROVE_V5] {
        let (_dir, db) = grove(grove_version);
        let result = db
            .apply_batch(
                vec![
                    at_key(Element::empty_tree()),
                    at_key(Element::empty_sum_tree()),
                    under_key(Element::new_sum_item(7)),
                ],
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            matches!(
                result,
                Err(Error::InvalidBatchOperation(
                    "batch operations fail consistency checks"
                ))
            ),
            "v{}: {result:?}",
            grove_version.protocol_version
        );
    }
}

/// `GROVE_V4` is what GroveDB 6.1.1 shipped and keeps applying these
/// batches, with the results below, so a replay stores what it stored.
#[test]
fn should_store_the_first_tree_type_for_a_tree_then_a_sum_tree_on_v4() {
    let grove_version = &GROVE_V4;
    let (_dir, db) = grove(grove_version);
    apply_without_consistency_check(
        &db,
        vec![
            at_key(Element::empty_tree()),
            at_key(Element::empty_sum_tree()),
            under_key(Element::new_sum_item(7)),
        ],
        grove_version,
    )
    .expect("expected the batch to apply");
    // The last insert asked for a sum tree: a plain tree is stored and the
    // sum is lost.
    assert_eq!(
        stored_at_key(&db, grove_version).expect("expected the tree"),
        Element::new_tree(Some(CHILD.to_vec()))
    );
    assert_eq!(issues(&db, grove_version), 0);
}

#[test]
fn should_store_a_tree_over_a_sum_tree_child_for_a_sum_tree_then_a_tree_on_v4() {
    let grove_version = &GROVE_V4;
    let (_dir, db) = grove(grove_version);
    apply_without_consistency_check(
        &db,
        vec![
            at_key(Element::empty_sum_tree()),
            at_key(Element::empty_tree()),
            under_key(Element::new_sum_item(7)),
        ],
        grove_version,
    )
    .expect("expected the batch to apply");
    assert_eq!(
        stored_at_key(&db, grove_version).expect("expected the tree"),
        Element::new_tree(Some(CHILD.to_vec()))
    );
    // The child was written as a sum tree under a plain tree element.
    assert_eq!(issues(&db, grove_version), 1);
}

/// Across the two segments of a partial batch the conflict never gets as far
/// as the reserved Merk: the cross-segment checks, which stay on with the
/// consistency check disabled, refuse an add-on tree insert over a tree the
/// first segment inserted, on every version.
#[test]
fn should_refuse_an_add_on_tree_of_another_type_over_a_first_segment_insert_on_every_version() {
    for grove_version in [&GROVE_V4, &GROVE_V5] {
        let (_dir, db) = grove(grove_version);
        let result = db
            .apply_partial_batch(
                vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![],
                    KEY.to_vec(),
                    Element::empty_tree(),
                )],
                Some(BatchApplyOptions {
                    disable_operation_consistency_check: true,
                    ..Default::default()
                }),
                |_cost, _leftover| {
                    Ok(vec![QualifiedGroveDbOp::insert_or_replace_op(
                        vec![],
                        KEY.to_vec(),
                        Element::empty_sum_tree(),
                    )])
                },
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            matches!(
                result,
                Err(Error::InvalidBatchOperation(
                    "add-on op replaces or deletes a subtree the initial segment wrote into"
                ))
            ),
            "v{}: {result:?}",
            grove_version.protocol_version
        );
        assert_eq!(issues(&db, grove_version), 0);
    }
}
