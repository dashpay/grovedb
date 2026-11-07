//! From GROVE_V5 a batch may carry the same keyless append twice (issue #1031):
//! every append family lands each one at its tree's next position, so the
//! batch must store exactly what the same appends applied one by one store.
//! GROVE_V4 refuses such a batch, as GroveDB 6.1.1 did
//! (`apply_batch.keyless_append_repeats` 0).

use grovedb_commitment_tree::{DashMemo, NoteBytesData, TransmittedNoteCiphertext};
use grovedb_version::version::{v4::GROVE_V4, GroveVersion};

use crate::{
    batch::QualifiedGroveDbOp,
    tests::{common::EMPTY_PATH, make_empty_grovedb, TempGroveDb},
    Element, Error,
};

const TREE_KEY: &[u8] = b"tree";
/// The number of times the batch carries the same append.
const REPEATS: usize = 3;

/// A database holding `tree` at [`TREE_KEY`].
fn db_with(tree: &Element) -> TempGroveDb {
    let db = make_empty_grovedb();
    db.insert(
        EMPTY_PATH,
        TREE_KEY,
        tree.clone(),
        None,
        None,
        GroveVersion::latest(),
    )
    .unwrap()
    .expect("insert the append tree");
    db
}

/// Applies `op` [`REPEATS`] times in one batch, with the consistency check on
/// (the default), and `append` as many times one by one, then checks both
/// databases agree: same root hash, so same contents.
fn assert_batched_repeats_match_sequential_appends(
    tree: Element,
    op: QualifiedGroveDbOp,
    append: impl Fn(&TempGroveDb),
) {
    let grove_version = GroveVersion::latest();

    let batched = db_with(&tree);
    batched
        .apply_batch(vec![op; REPEATS], None, None, grove_version)
        .unwrap()
        .expect("a batch carrying the same append several times applies");

    let sequential = db_with(&tree);
    for _ in 0..REPEATS {
        append(&sequential);
    }

    assert_eq!(
        batched.root_hash(None, grove_version).unwrap().unwrap(),
        sequential.root_hash(None, grove_version).unwrap().unwrap(),
        "the batch must store every repeated append, as sequential appends do"
    );
    assert!(batched
        .verify_grovedb(None, true, false, grove_version)
        .expect("verify_grovedb")
        .is_empty());
}

fn path() -> Vec<Vec<u8>> {
    vec![TREE_KEY.to_vec()]
}

/// One repeated append of each family, with the empty tree it goes to.
fn repeated_append_cases() -> Vec<(&'static str, Element, QualifiedGroveDbOp)> {
    let mut cmx = [0u8; 32];
    cmx[0] = 9;
    vec![
        (
            "private document store",
            Element::empty_private_document_store(24, 2).expect("valid config"),
            QualifiedGroveDbOp::private_document_store_insert_op(path(), vec![7u8; 24]),
        ),
        (
            "bulk append tree",
            Element::empty_bulk_append_tree(2).expect("valid chunk power"),
            QualifiedGroveDbOp::bulk_append_op(path(), b"value".to_vec()),
        ),
        (
            "mmr tree",
            Element::empty_mmr_tree(),
            QualifiedGroveDbOp::mmr_tree_append_op(path(), b"value".to_vec()),
        ),
        (
            "dense tree",
            Element::empty_dense_tree(3),
            QualifiedGroveDbOp::dense_tree_insert_op(path(), b"value".to_vec()),
        ),
        (
            "commitment tree",
            Element::empty_commitment_tree(2).expect("valid chunk power"),
            QualifiedGroveDbOp::commitment_tree_insert_op_typed(
                path(),
                cmx,
                [0xAA; 32],
                [0xCC; 32],
                &TransmittedNoteCiphertext::<DashMemo>::from_parts(
                    [1u8; 32],
                    NoteBytesData([2u8; 104]),
                    [3u8; 80],
                ),
            ),
        ),
    ]
}

/// A database holding `tree` at [`TREE_KEY`], under GROVE_V4.
fn db_with_v4(tree: &Element) -> TempGroveDb {
    let db = make_empty_grovedb();
    db.insert(EMPTY_PATH, TREE_KEY, tree.clone(), None, None, &GROVE_V4)
        .unwrap()
        .expect("insert the append tree");
    db
}

#[test]
fn should_refuse_a_batch_repeating_a_keyless_append_on_grove_v4() {
    for (case, tree, op) in repeated_append_cases() {
        let db = db_with_v4(&tree);
        let root_before = db.root_hash(None, &GROVE_V4).unwrap().unwrap();
        let result = db
            .apply_batch(vec![op; REPEATS], None, None, &GROVE_V4)
            .unwrap();
        assert!(
            matches!(result, Err(Error::InvalidBatchOperation(message)) if message.contains("fail consistency checks")),
            "{case}: GROVE_V4 must refuse the repeated append, got {result:?}"
        );
        assert_eq!(
            db.root_hash(None, &GROVE_V4).unwrap().unwrap(),
            root_before,
            "{case}"
        );
    }
}

/// An item at another root key, so a first segment writes nothing under the
/// append tree.
fn other_key_insert() -> QualifiedGroveDbOp {
    QualifiedGroveDbOp::insert_or_replace_op(
        vec![],
        b"other".to_vec(),
        Element::new_item(b"item".to_vec()),
    )
}

/// A partial batch checks its first segment and the add-on operations its
/// callback returns: GROVE_V4 refuses a repeated append in either, GROVE_V5
/// appends every copy.
#[test]
fn should_gate_repeated_appends_in_partial_batches_on_the_version() {
    for (case, tree, op) in repeated_append_cases() {
        // In the first segment.
        let db = db_with_v4(&tree);
        let result = db
            .apply_partial_batch(
                vec![op.clone(); REPEATS],
                None,
                |_, _| Ok(vec![]),
                None,
                &GROVE_V4,
            )
            .unwrap();
        assert!(
            matches!(result, Err(Error::InvalidBatchOperation(message)) if message.contains("fail consistency checks")),
            "{case}: GROVE_V4 must refuse the repeated append, got {result:?}"
        );
        let db = db_with(&tree);
        db.apply_partial_batch(
            vec![op.clone(); REPEATS],
            None,
            |_, _| Ok(vec![]),
            None,
            GroveVersion::latest(),
        )
        .unwrap()
        .unwrap_or_else(|e| panic!("{case}: GROVE_V5 must accept the repeated append: {e}"));

        // In the add-on operations, after a first segment writing another key
        // (an add-on append to a tree the first segment wrote is refused for
        // that alone).
        let db = db_with_v4(&tree);
        let add_on = op.clone();
        let result = db
            .apply_partial_batch(
                vec![other_key_insert()],
                None,
                |_, _| Ok(vec![add_on.clone(); REPEATS]),
                None,
                &GROVE_V4,
            )
            .unwrap();
        assert!(
            matches!(result, Err(Error::InvalidBatchOperation(message)) if message.contains("fail consistency checks")),
            "{case}: GROVE_V4 must refuse repeated add-on appends, got {result:?}"
        );
        let db = db_with(&tree);
        let add_on = op.clone();
        db.apply_partial_batch(
            vec![other_key_insert()],
            None,
            |_, _| Ok(vec![add_on.clone(); REPEATS]),
            None,
            GroveVersion::latest(),
        )
        .unwrap()
        .unwrap_or_else(|e| panic!("{case}: GROVE_V5 must accept repeated add-on appends: {e}"));
    }
}

#[test]
fn should_append_a_repeated_private_document_store_entry_as_many_times() {
    let entry = vec![7u8; 24];
    assert_batched_repeats_match_sequential_appends(
        Element::empty_private_document_store(24, 2).expect("valid config"),
        QualifiedGroveDbOp::private_document_store_insert_op(path(), entry.clone()),
        |db| {
            db.private_document_store_insert(
                EMPTY_PATH,
                TREE_KEY,
                entry.clone(),
                None,
                GroveVersion::latest(),
            )
            .unwrap()
            .expect("append");
        },
    );
    // The count shows every copy landed.
    let db = db_with(&Element::empty_private_document_store(24, 2).expect("valid config"));
    db.apply_batch(
        vec![QualifiedGroveDbOp::private_document_store_insert_op(path(), entry.clone()); REPEATS],
        None,
        None,
        GroveVersion::latest(),
    )
    .unwrap()
    .expect("apply");
    assert_eq!(
        db.private_document_store_count(EMPTY_PATH, TREE_KEY, None, GroveVersion::latest())
            .unwrap()
            .expect("count"),
        REPEATS as u64
    );
}

#[test]
fn should_append_a_repeated_bulk_append_value_as_many_times() {
    let value = b"value".to_vec();
    assert_batched_repeats_match_sequential_appends(
        Element::empty_bulk_append_tree(2).expect("valid chunk power"),
        QualifiedGroveDbOp::bulk_append_op(path(), value.clone()),
        |db| {
            db.bulk_append(
                EMPTY_PATH,
                TREE_KEY,
                value.clone(),
                None,
                GroveVersion::latest(),
            )
            .unwrap()
            .expect("append");
        },
    );
}

#[test]
fn should_append_a_repeated_mmr_value_as_many_times() {
    let value = b"value".to_vec();
    assert_batched_repeats_match_sequential_appends(
        Element::empty_mmr_tree(),
        QualifiedGroveDbOp::mmr_tree_append_op(path(), value.clone()),
        |db| {
            db.mmr_tree_append(
                EMPTY_PATH,
                TREE_KEY,
                value.clone(),
                None,
                GroveVersion::latest(),
            )
            .unwrap()
            .expect("append");
        },
    );
}

#[test]
fn should_append_a_repeated_dense_tree_value_as_many_times() {
    let value = b"value".to_vec();
    assert_batched_repeats_match_sequential_appends(
        Element::empty_dense_tree(3),
        QualifiedGroveDbOp::dense_tree_insert_op(path(), value.clone()),
        |db| {
            db.dense_tree_insert(
                EMPTY_PATH,
                TREE_KEY,
                value.clone(),
                None,
                GroveVersion::latest(),
            )
            .unwrap()
            .expect("append");
        },
    );
}

#[test]
fn should_append_a_repeated_commitment_tree_note_as_many_times() {
    let mut cmx = [0u8; 32];
    cmx[0] = 9;
    let rho = [0xAA; 32];
    let cv_net = [0xCC; 32];
    let ciphertext = || {
        TransmittedNoteCiphertext::<DashMemo>::from_parts(
            [1u8; 32],
            NoteBytesData([2u8; 104]),
            [3u8; 80],
        )
    };
    assert_batched_repeats_match_sequential_appends(
        Element::empty_commitment_tree(2).expect("valid chunk power"),
        QualifiedGroveDbOp::commitment_tree_insert_op_typed(
            path(),
            cmx,
            rho,
            cv_net,
            &ciphertext(),
        ),
        |db| {
            db.commitment_tree_insert(
                EMPTY_PATH,
                TREE_KEY,
                cmx,
                rho,
                cv_net,
                ciphertext(),
                None,
                GroveVersion::latest(),
            )
            .unwrap()
            .expect("append");
        },
    );
}
