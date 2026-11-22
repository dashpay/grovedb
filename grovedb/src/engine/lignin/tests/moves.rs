//! `GroveOp::Move` (GROVE-03): the same calls on LigninDB and on RocksDB,
//! with every answer, every `OperationCost` and the root hash after every
//! write compared, on the grove versions that take a move, and the
//! refusal of every move on those that take none.
//!
//! RocksDB copies the records of a moved tree to the prefixes of its new
//! path. LigninDB keeps every node where it is and gives the subtree its
//! new prefix (`tree-layout.md` D5): the engine walks the subtree for what
//! the copy is charged and writes none of its records.
//!
//! - `without_a_merk`: what a move carries that keeps no Merk of its own:
//!   items, numbers, references, and trees nothing was written into.
//! - `trees`: trees that hold elements, with trees in them. They need the
//!   transaction to give a subtree another prefix (`TXN-03-F4`).

use std::path::Path;

use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

use super::{
    batches::{delete, insert, path, reference, Calls, Op},
    nested::{assert_same, check, made_by, number, on_both_engines, open, Log, KINDS},
    LigninEngine,
};
use crate::{engine::Engine, reference_path::ReferencePathType, Element, Error};

fn rename(at: &[&[u8]], key: &[u8], new_key: &[u8]) -> Op {
    Op::move_op(path(at), key.to_vec(), new_key.to_vec())
}

/// Whether `version` takes a move in a batch.
fn moves(version: &GroveVersion) -> bool {
    version.grovedb_versions.apply_batch.move_element >= 1
}

/// A batch with a move: applied where `version` takes moves, and refused
/// where it takes none.
fn moving<E: Engine>(calls: &mut Calls<'_, '_, '_, E>, name: &str, ops: Vec<Op>) {
    match moves(calls.version) {
        true => calls.batch(name, ops),
        false => calls.refused(|calls| calls.batch(name, ops)),
    }
}

fn without_a_merk<E: Engine>(dir: &Path, version: &'static GroveVersion) -> Log {
    let db = open::<E>(dir);
    let takes = moves(version);
    let mut log = Log::default();
    let mut calls = Calls::new(&db, version, None, &mut log);
    let flags = Some(vec![7, 7, 7]);
    let ops = vec![
        insert(&[], b"plain", Element::empty_tree()),
        insert(&[], b"sums", Element::empty_sum_tree()),
        insert(&[], b"counts", Element::empty_count_tree()),
        insert(&[b"plain"], b"item", Element::new_item(vec![1; 40])),
        insert(&[b"plain"], b"long item", Element::new_item(vec![2; 400])),
        insert(
            &[b"plain"],
            b"flagged",
            Element::new_item_with_flags(vec![3; 20], flags.clone()),
        ),
        insert(&[b"plain"], b"stays", Element::new_item(vec![9; 9])),
        insert(&[b"plain"], b"to it", reference(&[b"plain", b"item"])),
        insert(
            &[b"plain"],
            b"beside",
            Element::new_reference(ReferencePathType::SiblingReference(b"stays".to_vec())),
        ),
        insert(&[b"plain"], b"never", Element::empty_tree()),
        insert(&[b"plain"], b"never sums", Element::empty_sum_tree()),
        insert(&[b"sums"], b"five", Element::new_sum_item(5)),
        insert(&[b"sums"], b"nine", Element::new_sum_item(9)),
        insert(&[b"counts"], b"one", Element::new_item(vec![4; 4])),
        insert(&[b"counts"], b"two", Element::new_item(vec![5; 5])),
    ];
    calls.batch("made", ops);

    // An item to a key of the same length, of a shorter one and of a
    // longer one, and a number in a tree that sums.
    moving(
        &mut calls,
        "an item",
        vec![rename(&[b"plain"], b"item", b"meti")],
    );
    moving(
        &mut calls,
        "to a shorter key",
        vec![rename(&[b"plain"], b"long item", b"l")],
    );
    moving(
        &mut calls,
        "to a longer key",
        vec![rename(&[b"plain"], b"l", b"a much longer key")],
    );
    moving(
        &mut calls,
        "a number",
        vec![rename(&[b"sums"], b"five", b"evif")],
    );
    moving(
        &mut calls,
        "in a tree that counts",
        vec![rename(&[b"counts"], b"one", b"eno")],
    );
    calls.get(&[], b"sums");
    calls.get(&[], b"counts");
    for key in [b"meti".as_slice(), b"a much longer key"] {
        match takes {
            true => calls.get(&[b"plain"], key),
            false => calls.refused(|calls| calls.get(&[b"plain"], key)),
        }
    }

    // An element with flags goes to a key of the same length and to no
    // other.
    moving(
        &mut calls,
        "with flags",
        vec![rename(&[b"plain"], b"flagged", b"deggalf")],
    );
    calls.refused(|calls| {
        let from: &[u8] = if takes { b"deggalf" } else { b"flagged" };
        calls.batch(
            "with flags, a longer key",
            vec![rename(&[b"plain"], from, b"longer than it")],
        );
    });

    // References: one that is moved, and one whose target is.
    moving(
        &mut calls,
        "a reference",
        vec![rename(&[b"plain"], b"beside", b"edeisb")],
    );
    calls.either(|calls| calls.get(&[b"plain"], b"edeisb"));
    calls.either(|calls| calls.get(&[b"plain"], b"to it"));

    // A tree nothing was written into, which is then written under its
    // new key, and one that is moved twice.
    moving(
        &mut calls,
        "an empty tree",
        vec![rename(&[b"plain"], b"never", b"moved")],
    );
    let under: &[u8] = if takes { b"moved" } else { b"never" };
    calls.batch(
        "written after it",
        vec![insert(
            &[b"plain", under],
            b"first",
            Element::new_item(vec![6; 6]),
        )],
    );
    calls.get(&[b"plain"], under);
    moving(
        &mut calls,
        "an empty sum tree",
        vec![rename(&[b"plain"], b"never sums", b"once")],
    );
    moving(
        &mut calls,
        "and again",
        vec![rename(&[b"plain"], b"once", b"twice")],
    );

    // Two elements that change places, one that moves to the key
    // another leaves, and an element with a tree nothing was written
    // into.
    let ops = vec![
        rename(&[b"sums"], b"evif", b"nine"),
        rename(&[b"sums"], b"nine", b"evif"),
    ];
    match takes {
        true => calls.batch("a swap", ops),
        false => calls.refused(|calls| calls.batch("a swap", ops)),
    }
    calls.either(|calls| calls.get(&[b"sums"], b"nine"));
    let ops = vec![
        rename(&[b"counts"], b"eno", b"two"),
        rename(&[b"counts"], b"two", b"three"),
    ];
    moving(&mut calls, "a chain", ops);
    calls.either(|calls| calls.get(&[b"counts"], b"three"));
    let ops = vec![
        rename(&[b"plain"], b"twice", b"meti"),
        rename(&[b"plain"], b"meti", b"twice"),
    ];
    moving(&mut calls, "an item and an empty tree", ops);

    // What a batch with a move is refused for.
    calls.refused(|calls| {
        calls.batch(
            "nothing to move",
            vec![rename(&[b"plain"], b"nothing", b"anything")],
        );
    });
    calls.refused(|calls| {
        calls.batch(
            "onto an element",
            vec![rename(&[b"counts"], b"three", b"two")],
        );
    });
    calls.refused(|calls| {
        calls.batch(
            "to its own key",
            vec![rename(&[b"counts"], b"three", b"three")],
        );
    });
    calls.refused(|calls| {
        let ops = vec![
            rename(&[b"counts"], b"three", b"four"),
            insert(&[b"counts"], b"four", Element::new_item(vec![1])),
        ];
        calls.batch("onto a key the batch writes", ops);
    });
    calls.refused(|calls| {
        let ops = vec![
            rename(&[], b"counts", b"stnuoc"),
            insert(&[b"counts"], b"more", Element::new_item(vec![1])),
        ];
        calls.batch("with a write under the moved key", ops);
    });
    calls.refused(|calls| {
        let ops = vec![
            rename(&[b"counts"], b"three", b"four"),
            delete(&[], b"counts"),
        ];
        calls.batch("in a tree the batch deletes", ops);
    });
    calls.refused(|calls| {
        let ops = vec![rename(&[b"counts"], b"three", b"four")];
        calls.partial("in a partial batch", ops, None, |_| Ok(Vec::new()));
    });
    // A refused batch has moved nothing.
    calls.either(|calls| calls.get(&[b"counts"], b"three"));
    calls.get(&[], b"counts");

    drop(calls);
    let transaction = db.start_transaction();
    let mut calls = Calls::new(&db, version, Some(&transaction), &mut log);
    // Inside a write transaction: an element is made and moved, a
    // savepoint is rolled back over a move, and the move is made again.
    calls.batch(
        "made in a transaction",
        vec![insert(
            &[b"plain"],
            b"fresh",
            Element::new_item(vec![8; 80]),
        )],
    );
    moving(
        &mut calls,
        "moved in it",
        vec![rename(&[b"plain"], b"fresh", b"hserf")],
    );
    db.set_savepoint(&transaction).expect("a savepoint");
    let from: &[u8] = if takes { b"hserf" } else { b"fresh" };
    moving(
        &mut calls,
        "to be undone",
        vec![rename(&[b"plain"], from, b"undone")],
    );
    db.rollback_to_savepoint(&transaction).expect("a rollback");
    calls.root();
    calls.get(&[b"plain"], from);
    moving(
        &mut calls,
        "in its place",
        vec![rename(&[b"plain"], from, b"placed")],
    );
    drop(calls);
    let committed = db.commit_transaction(transaction);
    log.answer("commit".to_owned(), committed);
    let mut calls = Calls::new(&db, version, None, &mut log);
    calls.root();
    calls.get(&[], b"plain");
    db.flush().expect("a flush");
    log
}

fn moves_without_a_merk_on(version: &'static GroveVersion) {
    on_both_engines!(without_a_merk, version);
}

#[test]
fn every_move_is_refused_alike_on_grove_v1() {
    moves_without_a_merk_on(&GROVE_VERSIONS[0]);
}

#[test]
fn every_move_is_refused_alike_on_grove_v2() {
    moves_without_a_merk_on(&GROVE_VERSIONS[1]);
}

#[test]
fn every_move_is_refused_alike_on_grove_v3() {
    moves_without_a_merk_on(&GROVE_VERSIONS[2]);
}

#[test]
fn elements_that_keep_no_merk_are_moved_on_grove_v4() {
    moves_without_a_merk_on(&GROVE_VERSIONS[3]);
}

#[test]
fn elements_that_keep_no_merk_are_moved_on_grove_v5() {
    moves_without_a_merk_on(&GROVE_VERSIONS[4]);
}

/// A tree of the kind `kind` under `at`, with what a move of it has to
/// carry: items, a number, a tree with a tree in it, a tree nothing was
/// written into, a tree that was emptied, and references that stay inside
/// it.
pub(super) fn filled(at: &[&[u8]], key: &[u8], kind: usize, made: usize) -> Vec<Op> {
    let (outer, inner) = (&KINDS[kind], &KINDS[(kind + 1) % made]);
    let own = [at, &[key]].concat();
    let nested = [own.as_slice(), &[b"nested"]].concat();
    let deeper = [nested.as_slice(), &[b"deeper"]].concat();
    vec![
        insert(at, key, (outer.make)()),
        insert(&own, b"item", Element::new_item(vec![kind as u8; 50])),
        insert(&own, b"long", Element::new_item(vec![kind as u8; 500])),
        insert(&own, b"number", number(outer.sums, 20 + kind as i64)),
        insert(&own, b"nested", (inner.make)()),
        insert(&own, b"never", (inner.make)()),
        insert(&own, b"emptied", Element::empty_tree()),
        insert(
            &[own.as_slice(), &[b"emptied"]].concat(),
            b"gone",
            Element::new_item(vec![1; 9]),
        ),
        insert(&nested, b"a", number(inner.sums, 3)),
        insert(&nested, b"b", Element::new_item(vec![7; 70])),
        insert(&nested, b"deeper", Element::empty_tree()),
        insert(&deeper, b"deep", Element::new_item(vec![9; 90])),
        insert(
            &own,
            b"inside",
            Element::new_reference(ReferencePathType::UpstreamFromElementHeightReference(
                0,
                vec![b"nested".to_vec(), b"b".to_vec()],
            )),
        ),
    ]
}

fn trees<E: Engine>(dir: &Path, version: &'static GroveVersion) -> Log {
    let db = open::<E>(dir);
    let made = made_by(version);
    let mut log = Log::default();
    let mut calls = Calls::new(&db, version, None, &mut log);
    // A tree of every kind, in a tree of the kind before it.
    for kind in 0..made {
        let parent = format!("parent {kind:02}").into_bytes();
        let parent = parent.as_slice();
        let mut ops = vec![insert(
            &[],
            parent,
            (KINDS[(kind + made - 1) % made].make)(),
        )];
        ops.extend(filled(&[parent], b"tree", kind, made));
        ops.extend(filled(&[parent], b"other", (kind + 2) % made, made));
        calls.batch("made", ops);
        calls.batch(
            "emptied",
            vec![
                delete(&[parent, b"tree", b"emptied"], b"gone"),
                delete(&[parent, b"other", b"emptied"], b"gone"),
            ],
        );
    }
    for kind in 0..made {
        let parent = format!("parent {kind:02}").into_bytes();
        let parent = parent.as_slice();
        // To a key of the same length, and what is read under both keys.
        calls.batch("a tree", vec![rename(&[parent], b"tree", b"eert")]);
        calls.get(&[parent], b"eert");
        calls.refused(|calls| calls.get(&[parent], b"tree"));
        calls.get(&[parent, b"eert", b"nested", b"deeper"], b"deep");
        calls.get(&[parent, b"eert"], b"inside");
        calls.is_empty(&[parent, b"eert", b"never"]);
        calls.is_empty(&[parent, b"eert", b"emptied"]);
        calls.refused(|calls| calls.get(&[parent, b"tree", b"nested"], b"a"));
        // It is written under its new key: in itself, in the tree in it,
        // and in the two trees that hold nothing.
        let ops = vec![
            insert(&[parent, b"eert"], b"item", Element::new_item(vec![5; 55])),
            insert(
                &[parent, b"eert", b"nested", b"deeper"],
                b"more",
                Element::new_item(vec![6; 6]),
            ),
            insert(
                &[parent, b"eert", b"never"],
                b"first",
                number(KINDS[(kind + 1) % made].sums, 1),
            ),
            insert(
                &[parent, b"eert", b"emptied"],
                b"again",
                Element::new_item(vec![2; 2]),
            ),
            delete(&[parent, b"eert", b"nested"], b"a"),
        ];
        calls.batch("written under its new key", ops);
        // To a longer key and to a shorter one, a swap with the other
        // tree, whose nested trees have the same keys, and a chain.
        calls.batch(
            "to a longer key",
            vec![rename(&[parent], b"eert", b"a longer key")],
        );
        calls.batch(
            "to a shorter key",
            vec![rename(&[parent], b"a longer key", b"t")],
        );
        calls.batch(
            "a swap",
            vec![
                rename(&[parent], b"t", b"other"),
                rename(&[parent], b"other", b"t"),
            ],
        );
        calls.get(&[parent, b"other", b"nested", b"deeper"], b"more");
        calls.refused(|calls| calls.get(&[parent, b"t", b"nested", b"deeper"], b"more"));
        let ops = vec![
            rename(&[parent], b"t", b"u"),
            rename(&[parent], b"other", b"t"),
        ];
        calls.batch("a chain", ops);
        calls.get(&[], parent);
        // A tree in the moved tree is moved inside it.
        calls.batch(
            "a tree in it",
            vec![rename(&[parent, b"t"], b"nested", b"detsen")],
        );
        calls.get(&[parent, b"t", b"detsen", b"deeper"], b"more");
        // A tree is inserted at a key a tree left, and written.
        let ops = vec![
            insert(&[parent], b"tree", (KINDS[kind].make)()),
            insert(&[parent, b"tree"], b"new", number(KINDS[kind].sums, 8)),
        ];
        calls.batch("a tree at the old key", ops);
        calls.refused(|calls| calls.get(&[parent, b"tree"], b"item"));
    }
    // A tree with a participant of backward references in it, and one
    // onto a tree that is there, are not moved.
    calls.refused(|calls| {
        calls.batch("onto a tree", vec![rename(&[b"parent 00"], b"t", b"u")]);
    });

    drop(calls);
    let transaction = db.start_transaction();
    let mut calls = Calls::new(&db, version, Some(&transaction), &mut log);
    let parent: &[u8] = b"parent 00";
    // Inside one write transaction: writes under the tree before it is
    // moved, the move, reads and writes under its new key, a second move,
    // a move that a savepoint takes back, and a move back to the first
    // key.
    let ops = vec![
        insert(&[parent, b"t"], b"before", Element::new_item(vec![1; 11])),
        insert(
            &[parent, b"t", b"never"],
            b"second",
            number(KINDS[1 % made].sums, 2),
        ),
    ];
    calls.batch("written before the move", ops);
    calls.batch(
        "moved in the transaction",
        vec![rename(&[parent], b"t", b"in")],
    );
    calls.get(&[parent, b"in"], b"before");
    calls.get(&[parent, b"in", b"never"], b"second");
    calls.refused(|calls| calls.get(&[parent, b"t"], b"before"));
    calls.is_empty(&[parent, b"in", b"detsen"]);
    let ops = vec![
        insert(&[parent, b"in"], b"after", Element::new_item(vec![3; 33])),
        delete(&[parent, b"in"], b"before"),
        insert(
            &[parent, b"in", b"detsen", b"deeper"],
            b"third",
            Element::new_item(vec![4; 4]),
        ),
    ];
    calls.batch("written after the move", ops);
    calls.batch("moved again", vec![rename(&[parent], b"in", b"on")]);
    db.set_savepoint(&transaction).expect("a savepoint");
    calls.batch("to be undone", vec![rename(&[parent], b"on", b"off")]);
    calls.get(&[parent, b"off"], b"after");
    db.rollback_to_savepoint(&transaction).expect("a rollback");
    calls.root();
    calls.get(&[parent, b"on"], b"after");
    calls.refused(|calls| calls.get(&[parent, b"off"], b"after"));
    calls.batch(
        "back to the first key",
        vec![rename(&[parent], b"on", b"t")],
    );
    drop(calls);
    let committed = db.commit_transaction(transaction);
    log.answer("commit".to_owned(), committed);

    let transaction = db.start_transaction();
    let mut calls = Calls::new(&db, version, Some(&transaction), &mut log);
    calls.root();
    calls.batch("moved and given up", vec![rename(&[parent], b"t", b"lost")]);
    db.rollback_transaction(&transaction).expect("a rollback");
    calls.root();
    calls.get(&[parent, b"t"], b"after");
    calls.batch("moved after it", vec![rename(&[parent], b"t", b"kept")]);
    drop(calls);
    let committed = db.commit_transaction(transaction);
    log.answer("commit".to_owned(), committed);
    let mut calls = Calls::new(&db, version, None, &mut log);
    calls.root();
    calls.get(&[parent, b"kept", b"detsen", b"deeper"], b"third");
    db.flush().expect("a flush");
    log
}

/// The history of moved trees makes no refusal but those it makes on
/// purpose, on the engine that copies.
#[cfg(feature = "rocksdb_storage")]
#[test]
fn the_history_of_moved_trees_is_sound_on_rocksdb() {
    for version in &GROVE_VERSIONS[3..] {
        let dir = tempfile::tempdir().expect("a directory");
        let log = trees::<crate::RocksDbEngine>(dir.path(), version);
        log.assert_refusals();
    }
}

fn moved_trees_on(version: &'static GroveVersion) {
    on_both_engines!(trees, version);
}

/// Until the transaction gives a subtree another prefix (`TXN-03-F4`),
/// LigninDB refuses a batch that moves a tree which was written into, and
/// takes nothing of it. It goes when the two tests below run.
#[test]
fn a_tree_that_was_written_into_is_not_moved_yet_and_stays_whole() {
    for version in [&GROVE_VERSIONS[3], &GROVE_VERSIONS[4]] {
        let dir = tempfile::tempdir().expect("a directory");
        let db = open::<LigninEngine>(dir.path());
        let made = filled(&[], b"tree", 0, made_by(version));
        (db.apply_batch(made, None, None, version).unwrap()).expect("a tree with elements");
        let root = db.root_hash(None, version).unwrap().expect("a root hash");
        let moved = db.apply_batch(vec![rename(&[], b"tree", b"eert")], None, None, version);
        let refused = moved.unwrap().expect_err("the move is refused");
        assert!(matches!(refused, Error::NotSupported(_)), "{refused:?}");
        assert!(
            refused.to_string().contains("the move of a subtree"),
            "{refused}"
        );
        let after = db.root_hash(None, version).unwrap().expect("a root hash");
        assert_eq!(after, root);
        let held = db.get(&[b"tree".as_slice(), b"nested"], b"b", None, version);
        held.unwrap().expect("what the tree held");
        drop(db);
        check(dir.path(), version);
    }
}

#[test]
#[ignore = "the transaction gives a subtree no other prefix yet (TXN-03-F4)"]
fn trees_are_moved_on_grove_v4() {
    moved_trees_on(&GROVE_VERSIONS[3]);
}

#[test]
#[ignore = "the transaction gives a subtree no other prefix yet (TXN-03-F4)"]
fn trees_are_moved_on_grove_v5() {
    moved_trees_on(&GROVE_VERSIONS[4]);
}
