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
//! - `trees`: trees that hold elements, with trees in them, which the
//!   transaction gives another prefix
//!   (`lignindb::txn::SubtreeWrite::Move`).
//! - `commits_beside_a_move`: what the commit of a mover and of a writer
//!   beside it meet, which is the conflict of the seam on both engines.
//! - `indexed_trees`: indexed trees, moved with the secondary of each
//!   axis.
//! - `flat_trees`, `flat_trees_below`: trees that keep their data flat,
//!   moved with their records, and in trees that are moved.
//! - `a_met_mover`: what a mover is answered after a commit beside it met
//!   its move, which differs between the engines before the commit.
//! - `moved_beside_deletes`: moves in a batch that deletes a tree with
//!   what it holds, and after such a delete.
//! - `records_that_no_tree_links_at_a_move`: records that no tree links,
//!   under trees whose element names a root and under trees whose element
//!   names none, which is the one place where the two engines leave
//!   different records behind.

use std::path::Path;

use grovedb_costs::OperationCost;
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

use super::{
    append_only::{
        self, verified, Answer, Appended, Bulk, Commitment, Dense, History, Mmr, Private,
    },
    batches::{delete, insert, path, reference, Calls, Op},
    indexed::{axis_reads, BigSum, Count, CountBigSum, CountSum, Indexed, Sum, SumAndAverage},
    nested::{assert_same, check, made_by, number, on_both_engines, open, Log, KINDS},
    Lignin, LigninEngine,
};
use crate::{
    engine::Engine, operations::delete::DeleteOptions, reference_path::ReferencePathType, Element,
    Error, GroveDb, TransactionArg,
};

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

/// What a commit meets beside the move of a tree, in three groves of
/// their own: the mover's commit once a commit beside it wrote into the
/// tree after the move, the mover's commit once a commit beside it put
/// an element under the tree's new key, and the commit of a transaction
/// that wrote into a tree in the moved tree before the mover committed.
/// Gives each commit with whether it was taken and whether its error is
/// the conflict of the seam.
fn commits_beside_a_move<E: Engine>(
    dir: &Path,
    version: &'static GroveVersion,
) -> Vec<(&'static str, bool, bool)> {
    let db = open::<E>(dir);
    let made = made_by(version);
    let item = || Element::new_item(vec![8; 18]);
    let groves: [&[u8]; 3] = [b"into", b"onto", b"under"];
    for grove in groves {
        let mut ops = vec![insert(&[], grove, Element::empty_tree())];
        ops.extend(filled(&[grove], b"t", 0, made));
        (db.apply_batch(ops, None, None, version).unwrap()).expect("a tree with elements");
    }
    let to_u = |grove: &[u8]| vec![rename(&[grove], b"t", b"u")];
    let mut seen = Vec::new();
    let mut commit = |case: &'static str, committed: Result<(), Error>| {
        let conflict = committed.as_ref().is_err_and(Error::is_commit_conflict);
        seen.push((case, committed.is_ok(), conflict));
    };

    let mover = db.start_transaction();
    (db.apply_batch(to_u(b"into"), None, Some(&mover), version)
        .unwrap())
    .expect("the move");
    let tree: &[&[u8]] = &[b"into", b"t"];
    (db.insert(tree, b"late", item(), None, None, version)
        .unwrap())
    .expect("a commit beside it");
    commit(
        "the mover, written into since",
        db.commit_transaction(mover).unwrap(),
    );

    let mover = db.start_transaction();
    (db.apply_batch(to_u(b"onto"), None, Some(&mover), version)
        .unwrap())
    .expect("the move");
    let above: &[&[u8]] = &[b"onto"];
    (db.insert(above, b"u", item(), None, None, version).unwrap()).expect("a commit beside it");
    commit(
        "the mover, its new key taken since",
        db.commit_transaction(mover).unwrap(),
    );

    let beside = db.start_transaction();
    let nested: &[&[u8]] = &[b"under", b"t", b"nested"];
    (db.insert(nested, b"late", item(), None, Some(&beside), version)
        .unwrap())
    .expect("a write beside the mover");
    let mover = db.start_transaction();
    (db.apply_batch(to_u(b"under"), None, Some(&mover), version)
        .unwrap())
    .expect("the move");
    commit("the mover", db.commit_transaction(mover).unwrap());
    commit(
        "the writer beside the mover",
        db.commit_transaction(beside).unwrap(),
    );
    db.flush().expect("a flush");
    seen
}

/// A commit that lands beside the move of a tree and wrote into the tree
/// or under its new key makes the mover's commit a conflict, and the
/// mover's commit makes the commit of a transaction that had written
/// into the tree one, on both engines. The LigninDB database is sound
/// afterwards.
#[test]
fn a_commit_beside_the_move_of_a_tree_is_a_conflict_on_both_engines() {
    let expected = [
        ("the mover, written into since", false, true),
        ("the mover, its new key taken since", false, true),
        ("the mover", true, false),
        ("the writer beside the mover", false, true),
    ];
    for version in [&GROVE_VERSIONS[3], &GROVE_VERSIONS[4]] {
        let dir = tempfile::tempdir().expect("a directory");
        let seen = commits_beside_a_move::<LigninEngine>(dir.path(), version);
        assert_eq!(seen, expected);
        check(dir.path(), version);
        #[cfg(feature = "rocksdb_storage")]
        {
            let dir = tempfile::tempdir().expect("a directory");
            let seen = commits_beside_a_move::<crate::RocksDbEngine>(dir.path(), version);
            assert_eq!(seen, expected);
        }
    }
}

/// A tree with elements changes places with a tree that was written into
/// and emptied. GroveDB asks no walk for the emptied one, whose Merk is
/// entered where the other goes: it is dropped with the move, and the
/// tree under each key is written afterwards. Then the tree with elements
/// changes places with an item.
fn swapped_with_an_emptied_tree<E: Engine>(dir: &Path, version: &'static GroveVersion) -> Log {
    let db = open::<E>(dir);
    let made = made_by(version);
    let mut log = Log::default();
    let mut calls = Calls::new(&db, version, None, &mut log);
    let item = |byte: u8| Element::new_item(vec![byte; 12]);
    let mut ops = filled(&[], b"full", 0, made);
    ops.push(insert(&[], b"hollow", Element::empty_tree()));
    ops.push(insert(&[b"hollow"], b"gone", item(1)));
    calls.batch("made", ops);
    calls.batch("emptied", vec![delete(&[b"hollow"], b"gone")]);
    let ops = vec![
        rename(&[], b"full", b"hollow"),
        rename(&[], b"hollow", b"full"),
    ];
    calls.batch("a swap", ops);
    calls.get(&[b"hollow", b"nested"], b"b");
    calls.get(&[b"hollow", b"nested", b"deeper"], b"deep");
    calls.is_empty(&[b"full"]);
    let ops = vec![
        insert(&[b"full"], b"new", item(2)),
        insert(&[b"hollow"], b"more", item(3)),
    ];
    calls.batch("written after the swap", ops);
    calls.get(&[b"full"], b"new");
    calls.get(&[b"hollow"], b"more");
    calls.proven(&[b"full"]);
    calls.proven(&[b"hollow"]);
    // The tree with elements changes places with an item: the node under
    // the key it leaves holds the item, and hosts nothing.
    calls.batch("an item", vec![insert(&[], b"item", item(4))]);
    let ops = vec![
        rename(&[], b"hollow", b"item"),
        rename(&[], b"item", b"hollow"),
    ];
    calls.batch("a swap with an item", ops);
    calls.get(&[], b"hollow");
    calls.get(&[b"item", b"nested"], b"b");
    calls.batch(
        "written after it",
        vec![insert(&[b"item"], b"last", item(5))],
    );
    calls.get(&[b"item"], b"last");
    calls.proven(&[b"item"]);
    db.flush().expect("a flush");
    log
}

#[test]
fn a_tree_changes_places_with_an_emptied_tree_on_grove_v4_and_v5() {
    for version in [&GROVE_VERSIONS[3], &GROVE_VERSIONS[4]] {
        on_both_engines!(swapped_with_an_emptied_tree, version);
    }
}

/// Runs `$history` on LigninDB and on RocksDB on the grove versions from
/// the `$first` one on, compares what the two engines answered, call for
/// call, and has the physical checker read what LigninDB holds. The
/// grove's own check, which the history runs once, finds nothing wrong.
macro_rules! moved_alike {
    ($history:path, $first:expr) => {{
        for (number, v) in GROVE_VERSIONS.iter().enumerate().skip($first - 1) {
            let number = number + 1;
            let dir = tempfile::tempdir().expect("a directory");
            let on_lignin = {
                let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
                let answers = $history(&db, v);
                db.flush().expect("a flush");
                answers
            };
            append_only::check(dir.path(), v);
            assert_eq!(append_only::taken(&on_lignin, "verified"), 1);
            assert!(append_only::sound(&on_lignin), "grove version {number}");
            #[cfg(feature = "rocksdb_storage")]
            {
                let dir = tempfile::tempdir().expect("a directory");
                let db = GroveDb::<crate::RocksDbEngine>::open_with_engine(dir.path())
                    .expect("a database on RocksDB");
                let on_rocksdb = $history(&db, v);
                assert_eq!(
                    on_lignin.len(),
                    on_rocksdb.len(),
                    "the calls on grove version {number}"
                );
                for (step, (lignin, rocksdb)) in on_lignin.iter().zip(&on_rocksdb).enumerate() {
                    assert_eq!(lignin, rocksdb, "call {step} on grove version {number}");
                }
            }
        }
    }};
}

/// A batch that moves, which is applied, and the root hash after it.
fn moved<'db, E: Engine>(
    h: &mut History<'db, E>,
    ops: Vec<Op>,
    through: TransactionArg<'db, '_, E>,
) {
    let applied = h.db.apply_batch(ops, None, through, h.v);
    (h.write_in("moved", applied, through)).expect("the batch that moves is applied");
}

/// The values `values` appended to the tree `key` under `path`, each
/// taken, and every read of the tree's own file after them.
fn appended<'db, T: Appended, E: Engine>(
    h: &mut History<'db, E>,
    path: &[&[u8]],
    key: &[u8],
    values: std::ops::Range<usize>,
    through: TransactionArg<'db, '_, E>,
) {
    for n in values.clone() {
        let appended = T::append(h.db, path, key, n, through, h.v);
        (h.write_in("append", appended, through)).expect("an append");
    }
    T::reads(h, path, key, values.end as u64, through);
}

/// Trees of one kind that keeps its data flat, moved with their records.
///
/// Under a tree: one with values, one nothing was appended to, a tree
/// with an element and a tree of the kind in it, and a tree that was
/// written into and emptied. The one with values goes to a longer key and
/// is read under both keys and appended to; the one nothing was appended
/// to is moved and appended to; the tree with one of the kind in it is
/// moved; it changes places with the one of the kind, and that one with
/// the emptied tree, each written afterwards. Then in a write
/// transaction: a move, an append, a move that a savepoint takes back,
/// and the commit.
fn flat_trees<T: Appended, E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    let mut h = History::new(db, v);
    let item = || Element::new_item(vec![0x4d; 12]);
    let p: &[&[u8]] = &[b"p"];
    let tree: &[&[u8]] = &[b"p", b"tree"];
    let emptied: &[&[u8]] = &[b"p", b"emptied"];
    let made = vec![
        insert(&[], b"p", Element::empty_tree()),
        insert(p, b"full", T::empty(None)),
        insert(p, b"never", T::empty(None)),
        insert(p, b"tree", Element::empty_tree()),
        insert(tree, b"x", item()),
        insert(tree, b"in", T::empty(None)),
        insert(p, b"emptied", Element::empty_tree()),
        insert(emptied, b"gone", item()),
    ];
    (h.write("made", db.apply_batch(made, None, None, v))).expect("the trees are made");
    (h.write("emptied", db.delete(emptied, b"gone", None, None, v))).expect("a delete");
    let full = T::DIRECT;
    appended::<T, E>(&mut h, p, b"full", 0..full, None);
    appended::<T, E>(&mut h, tree, b"in", 0..3, None);

    // To a longer key: what both keys answer, and values under the new
    // one.
    moved(&mut h, vec![rename(p, b"full", b"a longer key")], None);
    T::reads(&mut h, p, b"full", full as u64, None);
    appended::<T, E>(&mut h, p, b"a longer key", full..full + 2, None);
    // The tree nothing was appended to, and its first values.
    moved(&mut h, vec![rename(p, b"never", b"n")], None);
    appended::<T, E>(&mut h, p, b"n", 0..2, None);
    // The tree with a tree of the kind in it.
    moved(&mut h, vec![rename(p, b"tree", b"eert")], None);
    let eert: &[&[u8]] = &[b"p", b"eert"];
    h.element("get", db.get(eert, b"x", None, v));
    appended::<T, E>(&mut h, eert, b"in", 3..5, None);
    // It changes places with the tree of the kind, whose subtree is
    // entered and has no root.
    let ops = vec![
        rename(p, b"a longer key", b"eert"),
        rename(p, b"eert", b"a longer key"),
    ];
    moved(&mut h, ops, None);
    let longer: &[&[u8]] = &[b"p", b"a longer key"];
    appended::<T, E>(&mut h, p, b"eert", full + 2..full + 3, None);
    h.element("get", db.get(longer, b"x", None, v));
    appended::<T, E>(&mut h, longer, b"in", 5..6, None);
    // The tree of the kind changes places with the emptied tree, and both
    // are written.
    let ops = vec![
        rename(p, b"eert", b"emptied"),
        rename(p, b"emptied", b"eert"),
    ];
    moved(&mut h, ops, None);
    appended::<T, E>(&mut h, p, b"emptied", full + 3..full + 4, None);
    (h.write("insert", db.insert(eert, b"again", item(), None, None, v))).expect("an insert");
    h.element("get", db.get(eert, b"again", None, v));

    let tx = db.start_transaction();
    moved(&mut h, vec![rename(p, b"emptied", b"in tx")], Some(&tx));
    appended::<T, E>(&mut h, p, b"in tx", full + 4..full + 5, Some(&tx));
    T::reads(&mut h, p, b"emptied", full as u64 + 4, None);
    h.plain("savepoint", db.set_savepoint(&tx));
    moved(&mut h, vec![rename(p, b"in tx", b"undone")], Some(&tx));
    T::reads(&mut h, p, b"undone", full as u64 + 5, Some(&tx));
    h.plain("rollback to savepoint", db.rollback_to_savepoint(&tx));
    h.read("root", db.root_hash(Some(&tx), v));
    T::reads(&mut h, p, b"undone", full as u64 + 5, Some(&tx));
    appended::<T, E>(&mut h, p, b"in tx", full + 5..full + 6, Some(&tx));
    (h.write("commit", db.commit_transaction(tx))).expect("a commit");
    T::reads(&mut h, p, b"in tx", full as u64 + 6, None);
    verified(&mut h);
    h.answers
}

/// Indexed trees of one kind, moved with their primary and the secondary
/// of each axis.
///
/// Under a tree: one with children, of which one is a tree with elements,
/// another with children, and one nothing was written into. The first
/// goes to a longer key, is read on every axis and written under its new
/// key, at the primary and below it; the one nothing was written into is
/// moved and written; the two with children change places; and the tree
/// above them is moved, with every Merk of each below it.
fn indexed_trees<T: Indexed, E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    let mut h = History::new(db, v);
    let leaf_key = |n: usize| format!("leaf{n:02}").into_bytes();
    let root: &[&[u8]] = &[];
    let p: &[&[u8]] = &[b"p"];
    let idx: &[&[u8]] = &[b"p", b"idx"];
    let other: &[&[u8]] = &[b"p", b"other"];
    let in_b0: &[&[u8]] = &[b"p", b"idx", b"b0"];
    let made = db.insert(root, b"p", Element::empty_tree(), None, None, v);
    h.write("made", made).expect("a tree");
    for key in [b"idx".as_slice(), b"other", b"never"] {
        (h.write("made", db.insert(p, key, T::empty(), None, None, v))).expect("an indexed tree");
    }
    let child = |h: &mut History<'_, E>, at: &[&[u8]], n: usize| {
        let inserted = T::insert(db, at, &leaf_key(n), T::leaf(n), None, v);
        h.write("insert", inserted).expect("a child");
    };
    let below = |h: &mut History<'_, E>, at: &[&[u8]], n: usize| {
        let inserted = db.insert(at, &leaf_key(n), T::in_branch(n), None, None, v);
        h.write("insert", inserted)
            .expect("an element below the primary");
    };
    for n in 0..6 {
        child(&mut h, idx, n);
    }
    (h.write("insert", T::insert(db, idx, b"b0", T::branch(), None, v))).expect("a branch");
    for n in 0..3 {
        below(&mut h, in_b0, n);
    }
    for n in 6..9 {
        child(&mut h, other, n);
    }
    axis_reads::<T, E>(&mut h, idx);

    // To a longer key: every axis under the new key, and what the old one
    // answers.
    moved(&mut h, vec![rename(p, b"idx", b"a longer key")], None);
    let longer: &[&[u8]] = &[b"p", b"a longer key"];
    axis_reads::<T, E>(&mut h, longer);
    h.element("get", db.get(p, b"idx", None, v));
    h.read("empty", db.is_empty_tree(idx, None, v));
    // Written under its new key: a child more, a child less, and an
    // element below the primary, each of which moves rows of the axes.
    child(&mut h, longer, 9);
    let deleted = T::delete(db, longer, &leaf_key(1), None, v);
    h.write("delete", deleted).expect("a child is deleted");
    below(&mut h, &[b"p", b"a longer key", b"b0"], 5);
    axis_reads::<T, E>(&mut h, longer);
    // The tree nothing was written into, and its first children.
    moved(&mut h, vec![rename(p, b"never", b"n")], None);
    let n: &[&[u8]] = &[b"p", b"n"];
    child(&mut h, n, 2);
    child(&mut h, n, 3);
    axis_reads::<T, E>(&mut h, n);
    // The two with children change places.
    let ops = vec![
        rename(p, b"a longer key", b"other"),
        rename(p, b"other", b"a longer key"),
    ];
    moved(&mut h, ops, None);
    axis_reads::<T, E>(&mut h, other);
    axis_reads::<T, E>(&mut h, longer);
    child(&mut h, other, 10);
    child(&mut h, longer, 11);
    // The tree above them.
    moved(&mut h, vec![rename(root, b"p", b"q")], None);
    let other: &[&[u8]] = &[b"q", b"other"];
    axis_reads::<T, E>(&mut h, other);
    child(&mut h, other, 12);
    below(&mut h, &[b"q", b"other", b"b0"], 7);
    axis_reads::<T, E>(&mut h, other);
    axis_reads::<T, E>(&mut h, &[b"q", b"a longer key"]);
    h.read("empty", db.is_empty_tree(p, None, v));
    verified(&mut h);
    h.answers
}

/// Trees of one kind that keeps its data flat, in trees that are moved,
/// and one nothing was appended to, which has no subtree.
///
/// Under a tree: one of the kind that nothing was appended to, a tree
/// with an element and two of the kind in it, of which one has values,
/// and a tree of another type with one of the kind under the same key.
/// The one nothing was appended to is moved and appended to; the tree
/// with the two in it goes to a longer key, and both are appended to
/// there; the two trees change places; then, in a write transaction, a
/// move, an append, a move that a savepoint takes back, and the commit.
fn flat_trees_below<T: Appended, E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    let mut h = History::new(db, v);
    let item = || Element::new_item(vec![0x4d; 12]);
    let p: &[&[u8]] = &[b"p"];
    let tree: &[&[u8]] = &[b"p", b"tree"];
    let other: &[&[u8]] = &[b"p", b"other"];
    let made = vec![
        insert(&[], b"p", Element::empty_tree()),
        insert(p, b"never", T::empty(None)),
        insert(p, b"tree", Element::empty_tree()),
        insert(tree, b"x", item()),
        insert(tree, b"in", T::empty(None)),
        insert(tree, b"unused", T::empty(None)),
        insert(p, b"other", Element::empty_count_tree()),
        insert(other, b"in", T::empty(None)),
    ];
    (h.write("made", db.apply_batch(made, None, None, v))).expect("the trees are made");
    let full = T::DIRECT;
    appended::<T, E>(&mut h, tree, b"in", 0..full, None);
    appended::<T, E>(&mut h, other, b"in", 0..3, None);

    // The tree nothing was appended to, and its first values.
    moved(&mut h, vec![rename(p, b"never", b"n")], None);
    appended::<T, E>(&mut h, p, b"n", 0..2, None);
    // The tree with the two of the kind in it, to a longer key: what the
    // old path answers, and values under the new one.
    moved(&mut h, vec![rename(p, b"tree", b"a longer key")], None);
    let longer: &[&[u8]] = &[b"p", b"a longer key"];
    T::reads(&mut h, tree, b"in", full as u64, None);
    h.element("get", db.get(longer, b"x", None, v));
    appended::<T, E>(&mut h, longer, b"in", full..full + 2, None);
    appended::<T, E>(&mut h, longer, b"unused", 0..1, None);
    // The two trees change places, each with a tree of the kind under
    // the key `in`.
    let ops = vec![
        rename(p, b"a longer key", b"other"),
        rename(p, b"other", b"a longer key"),
    ];
    moved(&mut h, ops, None);
    appended::<T, E>(&mut h, other, b"in", full + 2..full + 3, None);
    appended::<T, E>(&mut h, longer, b"in", 3..4, None);
    h.element("get", db.get(other, b"x", None, v));

    let tx = db.start_transaction();
    let in_tx: &[&[u8]] = &[b"p", b"in tx"];
    let undone: &[&[u8]] = &[b"p", b"undone"];
    moved(&mut h, vec![rename(p, b"other", b"in tx")], Some(&tx));
    appended::<T, E>(&mut h, in_tx, b"in", full + 3..full + 4, Some(&tx));
    T::reads(&mut h, other, b"in", full as u64 + 3, None);
    h.plain("savepoint", db.set_savepoint(&tx));
    moved(&mut h, vec![rename(p, b"in tx", b"undone")], Some(&tx));
    T::reads(&mut h, undone, b"in", full as u64 + 4, Some(&tx));
    h.plain("rollback to savepoint", db.rollback_to_savepoint(&tx));
    h.read("root", db.root_hash(Some(&tx), v));
    T::reads(&mut h, undone, b"in", full as u64 + 4, Some(&tx));
    appended::<T, E>(&mut h, in_tx, b"in", full + 4..full + 5, Some(&tx));
    (h.write("commit", db.commit_transaction(tx))).expect("a commit");
    T::reads(&mut h, in_tx, b"in", full as u64 + 5, None);
    verified(&mut h);
    h.answers
}

/// A tree of leaves with a leaf is deleted by a batch, which removes its
/// element and its records, and a tree with elements is moved to its key
/// and written there.
fn onto_the_key_of_a_deleted_tree_of_leaves<E: Engine>(
    dir: &Path,
    version: &'static GroveVersion,
) -> Log {
    use crate::{batch::SubelementsDeletionBehavior::DeleteChildren, TreeType};

    let db = open::<E>(dir);
    let mut log = Log::default();
    let mut calls = Calls::new(&db, version, None, &mut log);
    let root: &[&[u8]] = &[];
    let mut ops = filled(&[], b"t", 0, made_by(version));
    ops.push(insert(&[], b"u", Element::empty_mmr_tree()));
    calls.batch("made", ops);
    let appended = db.mmr_tree_append(root, b"u", vec![7; 40], None, version);
    calls.log.answer("append".to_owned(), appended);
    calls.root();
    let deleted = Op::delete_tree_op(path(&[]), b"u".to_vec(), TreeType::MmrTree, DeleteChildren);
    calls.batch("deleted", vec![deleted]);
    calls.refused(|calls| calls.get(&[], b"u"));
    calls.batch("onto its key", vec![rename(&[], b"t", b"u")]);
    calls.get(&[b"u", b"nested"], b"b");
    calls.refused(|calls| calls.get(&[], b"t"));
    let more = Element::new_item(vec![2; 12]);
    calls.batch("written there", vec![insert(&[b"u"], b"new", more)]);
    calls.get(&[b"u"], b"new");
    calls.proven(&[b"u"]);
    db.flush().expect("a flush");
    log
}

#[test]
fn a_tree_is_moved_to_the_key_of_a_deleted_tree_of_leaves_on_grove_v4_and_v5() {
    for version in [&GROVE_VERSIONS[3], &GROVE_VERSIONS[4]] {
        on_both_engines!(onto_the_key_of_a_deleted_tree_of_leaves, version);
    }
}

/// What changes places with the emptied indexed tree.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Filled {
    /// A tree with elements.
    Tree,
    /// A tree indexed by count and by sum, with a row.
    Indexed,
}

/// A filled tree under `full` and under `idx` a tree indexed by count
/// that was written into and emptied, the batch that makes them change
/// places, and a child of the emptied tree under its new key: the cost of
/// the batch and of the insert, and the root hash after each.
fn swapped_with_an_emptied_indexed_tree<E: Engine>(
    dir: &Path,
    filled_tree: Filled,
    version: &'static GroveVersion,
) -> [(OperationCost, [u8; 32]); 2] {
    let db = open::<E>(dir);
    let root: &[&[u8]] = &[];
    let held: &[u8] = match filled_tree {
        Filled::Tree => {
            let made = filled(&[], b"full", 0, made_by(version));
            (db.apply_batch(made, None, None, version).unwrap()).expect("a tree with elements");
            b"nested"
        }
        Filled::Indexed => {
            (db.insert(root, b"full", CountSum::empty(), None, None, version))
                .unwrap()
                .expect("an indexed tree");
            let row = CountSum::leaf(3);
            (CountSum::insert(&db, &[b"full"], b"row", row, None, version).unwrap())
                .expect("a row");
            b"row"
        }
    };
    let indexed = Count::empty();
    (db.insert(root, b"idx", indexed, None, None, version))
        .unwrap()
        .expect("an indexed tree");
    (Count::insert(&db, &[b"idx"], b"row", Count::leaf(1), None, version).unwrap())
        .expect("a child");
    (Count::delete(&db, &[b"idx"], b"row", None, version).unwrap()).expect("no child");
    let root_hash = || db.root_hash(None, version).unwrap().expect("a root hash");
    let before = root_hash();
    let ops = vec![rename(&[], b"full", b"idx"), rename(&[], b"idx", b"full")];
    let swapped = db.apply_batch(ops, None, None, version);
    swapped.value.expect("the two change places");
    let swapped = (swapped.cost, root_hash());
    assert_ne!(swapped.1, before, "the swap moved no root hash");
    let read = db.get(&[b"idx".as_slice()], held, None, version);
    read.unwrap()
        .expect("the filled tree is read under its new key");
    let child = Count::insert(&db, &[b"full"], b"row", Count::leaf(2), None, version);
    child
        .value
        .expect("a child of the emptied tree under its new key");
    let child = (child.cost, root_hash());
    db.flush().expect("a flush");
    [swapped, child]
}

/// An indexed tree that was written into and emptied keeps its primary
/// and the secondary of each axis entered, with no node. A filled tree
/// that changes places with it comes to the prefix of that primary: the
/// primary and its secondaries make room and are dropped with the batch
/// that moves the filled tree there, a secondary too where a secondary of
/// the filled tree comes to its prefix. The emptied tree has nothing
/// entered under its new key, and its first write there enters its Merks.
/// The costs and the root hashes are RocksDB's and the store is sound.
#[test]
#[cfg_attr(not(feature = "rocksdb_storage"), allow(unused_variables))]
fn a_tree_changes_places_with_an_emptied_indexed_tree() {
    use grovedb_element::indexed::IndexAxis;
    use grovedb_storage::prefix::{build_prefix, secondary_prefix_for};

    use super::indexed::entry;

    let prefix = |key: &[u8]| build_prefix([key].as_slice().into()).value;
    let of = |axis: IndexAxis, primary: [u8; 32]| secondary_prefix_for(&primary, axis.tag()).value;
    let (idx, full) = (prefix(b"idx"), prefix(b"full"));
    for (filled_tree, version) in [Filled::Tree, Filled::Indexed]
        .into_iter()
        .flat_map(|filled_tree| [3, 4].map(|at| (filled_tree, &GROVE_VERSIONS[at])))
    {
        let dir = tempfile::tempdir().expect("a directory");
        let on_lignin =
            swapped_with_an_emptied_indexed_tree::<LigninEngine>(dir.path(), filled_tree, version);
        {
            let db = open::<LigninEngine>(dir.path());
            // What is entered under each key, by position, and that each
            // has a root; and what is not entered at all.
            let (under_idx, absent) = match filled_tree {
                Filled::Tree => (vec![idx], vec![of(IndexAxis::Count, idx)]),
                Filled::Indexed => (
                    vec![
                        idx,
                        of(IndexAxis::Count, idx),
                        of(IndexAxis::Sum, idx),
                        of(IndexAxis::Avg, idx),
                    ],
                    vec![of(IndexAxis::Sum, full), of(IndexAxis::Avg, full)],
                ),
            };
            let under_full = vec![full, of(IndexAxis::Count, full)];
            for (key, merks) in [(&b"idx"[..], under_idx), (&b"full"[..], under_full)] {
                for (place, merk) in merks.into_iter().enumerate() {
                    let entered = entry(&db, merk).expect("a Merk under the key");
                    let host = entered.host.expect("a host");
                    assert_eq!(
                        (host.key.as_slice(), usize::from(host.position)),
                        (key, place),
                        "{filled_tree:?}"
                    );
                    assert!(entered.root_key.is_some(), "{filled_tree:?}");
                }
            }
            let moved = entry(&db, idx).expect("the filled tree");
            let indexed = moved.tree_type().expect("a tree type").is_indexed_primary();
            assert_eq!(indexed, filled_tree == Filled::Indexed);
            for merk in absent {
                assert_eq!(entry(&db, merk), None, "{filled_tree:?}");
            }
            db.flush().expect("a flush");
        }
        check(dir.path(), version);
        #[cfg(feature = "rocksdb_storage")]
        {
            let dir = tempfile::tempdir().expect("a directory");
            let on_rocksdb = swapped_with_an_emptied_indexed_tree::<crate::RocksDbEngine>(
                dir.path(),
                filled_tree,
                version,
            );
            assert_eq!(on_lignin, on_rocksdb, "{filled_tree:?}");
        }
    }
}

/// What a mover is answered after a commit beside it met its move: a
/// write into a tree that the move does not name, an item over a tree
/// with elements, which drops a subtree on LigninDB, the clear of a tree
/// with elements, the delete of a tree with its elements, which drops a
/// subtree on LigninDB too, and its commit. Each with whether it was
/// taken and whether its error is the conflict of the seam.
fn a_met_mover<E: Engine>(
    dir: &Path,
    version: &'static GroveVersion,
) -> Vec<(&'static str, bool, bool)> {
    let db = open::<E>(dir);
    let item = || Element::new_item(vec![8; 18]);
    let mut ops = vec![insert(&[], b"met", Element::empty_tree())];
    ops.extend(filled(&[b"met"], b"t", 0, made_by(version)));
    for tree in [b"b".as_slice(), b"c", b"d", b"e"] {
        ops.push(insert(&[b"met"], tree, Element::empty_tree()));
        ops.push(insert(&[b"met", tree], b"x", item()));
    }
    (db.apply_batch(ops, None, None, version).unwrap()).expect("trees with elements");
    let mut seen = Vec::new();
    let mut answered = |case: &'static str, answer: Result<(), Error>| {
        let conflict = answer.as_ref().is_err_and(Error::is_commit_conflict);
        seen.push((case, answer.is_ok(), conflict));
    };

    let mover = db.start_transaction();
    let moved = vec![rename(&[b"met"], b"t", b"u")];
    (db.apply_batch(moved, None, Some(&mover), version).unwrap()).expect("the move");
    let tree: &[&[u8]] = &[b"met", b"t"];
    (db.insert(tree, b"late", item(), None, None, version)
        .unwrap())
    .expect("a commit beside it");
    let beside: &[&[u8]] = &[b"met", b"c"];
    let written = db.insert(beside, b"y", item(), None, Some(&mover), version);
    answered("a write into another tree", written.unwrap());
    let over = vec![insert(&[b"met"], b"b", item())];
    let dropped = db.apply_batch(over, None, Some(&mover), version);
    answered("an item over a tree", dropped.unwrap());
    let cleared: &[&[u8]] = &[b"met", b"d"];
    let cleared = db.clear_subtree(cleared, None, Some(&mover), version);
    answered(
        "a clear of a tree",
        cleared.map(|cleared| assert!(cleared, "the tree holds no tree")),
    );
    let above: &[&[u8]] = &[b"met"];
    let with_its_elements = Some(DeleteOptions {
        allow_deleting_non_empty_trees: true,
        deleting_non_empty_trees_returns_error: false,
        ..Default::default()
    });
    let deleted = db.delete(above, b"e", with_its_elements, Some(&mover), version);
    answered("a delete of a tree", deleted.unwrap());
    answered("the commit", db.commit_transaction(mover).unwrap());
    db.flush().expect("a flush");
    seen
}

/// The commit of a mover that a commit beside it met fails as a conflict
/// on both engines. RocksDB takes every write of the mover until then.
/// On LigninDB a transaction that a commit met serves its records by
/// key, and while it holds a move it is refused every drop of a subtree
/// (`transactions.md`, MV7): the item over a tree and the delete of a
/// tree with its elements, each of which drops a subtree, are refused as
/// conflicts before the commit is. The writes that drop nothing, into
/// trees that the move does not name, are taken as on RocksDB: the
/// write of an item, and the clear of a tree, which removes the tree's
/// nodes by their keys and states no clear to the transaction.
#[test]
fn a_mover_that_a_commit_met_is_refused_a_drop_before_its_commit_is() {
    let on_lignindb = [
        ("a write into another tree", true, false),
        ("an item over a tree", false, true),
        ("a clear of a tree", true, false),
        ("a delete of a tree", false, true),
        ("the commit", false, true),
    ];
    for version in [&GROVE_VERSIONS[3], &GROVE_VERSIONS[4]] {
        let dir = tempfile::tempdir().expect("a directory");
        let seen = a_met_mover::<LigninEngine>(dir.path(), version);
        assert_eq!(seen, on_lignindb);
        check(dir.path(), version);
        #[cfg(feature = "rocksdb_storage")]
        {
            let on_rocksdb = [
                ("a write into another tree", true, false),
                ("an item over a tree", true, false),
                ("a clear of a tree", true, false),
                ("a delete of a tree", true, false),
                ("the commit", false, true),
            ];
            let dir = tempfile::tempdir().expect("a directory");
            let seen = a_met_mover::<crate::RocksDbEngine>(dir.path(), version);
            assert_eq!(seen, on_rocksdb);
        }
    }
}

/// A tree with an item, a tree in it with an item, and a tree in that
/// with an item, under `key` at `at`.
fn three_deep(at: &[&[u8]], key: &[u8]) -> Vec<Op> {
    let item = |byte: u8| Element::new_item(vec![byte; 14]);
    let own = [at, &[key]].concat();
    let inner = [own.as_slice(), &[b"inner"]].concat();
    let deeper = [inner.as_slice(), &[b"deeper"]].concat();
    vec![
        insert(at, key, Element::empty_tree()),
        insert(&own, b"x", item(1)),
        insert(&own, b"inner", Element::empty_tree()),
        insert(&inner, b"y", item(2)),
        insert(&inner, b"deeper", Element::empty_tree()),
        insert(&deeper, b"z", item(3)),
    ]
}

/// Moves beside the deletes of trees that hold elements, each deleted
/// with the trees in it. A tree in a tree is deleted and the tree above
/// it is moved by the next batch. One batch deletes a tree and moves the
/// tree beside it, with the delete first and with the move first; one
/// moves a tree to the key that a batch before it deleted a tree under;
/// one deletes a tree in one tree and moves a tree in another; and one
/// deletes a tree in the tree beside the moved one. Then the same two
/// kinds of batch in one write transaction. What GroveDB lets no batch
/// do with a move is refused before the engine is asked: a delete at the
/// key a move goes to, a delete under the key it leaves, and a delete of
/// the tree it is made in.
fn moved_beside_deletes<E: Engine>(dir: &Path, version: &'static GroveVersion) -> Log {
    use crate::{batch::SubelementsDeletionBehavior::DeleteChildren, TreeType};

    let db = open::<E>(dir);
    let mut log = Log::default();
    let mut calls = Calls::new(&db, version, None, &mut log);
    let item = |byte: u8| Element::new_item(vec![byte; 14]);
    let felled = |at: &[&[u8]], key: &[u8]| {
        Op::delete_tree_op(path(at), key.to_vec(), TreeType::NormalTree, DeleteChildren)
    };
    let mut ops = vec![
        insert(&[], b"p", Element::empty_tree()),
        insert(&[], b"q", Element::empty_tree()),
    ];
    for key in [b"a".as_slice(), b"b", b"c", b"d"] {
        ops.extend(three_deep(&[b"p"], key));
    }
    for key in [b"e".as_slice(), b"f"] {
        ops.extend(three_deep(&[b"q"], key));
    }
    calls.batch("made", ops);

    // The tree in a tree goes with the tree in it, and the tree above is
    // moved by the next batch.
    calls.batch("a tree in a tree", vec![felled(&[b"p", b"a"], b"inner")]);
    calls.batch("the tree above it", vec![rename(&[b"p"], b"a", b"a2")]);
    calls.get(&[b"p", b"a2"], b"x");
    calls.refused(|calls| calls.get(&[b"p", b"a2"], b"inner"));
    calls.refused(|calls| calls.get(&[b"p", b"a"], b"x"));
    let ops = vec![
        insert(&[b"p", b"a2"], b"inner", Element::empty_tree()),
        insert(&[b"p", b"a2", b"inner"], b"y", item(4)),
    ];
    calls.batch("a tree made again in it", ops);
    calls.get(&[b"p", b"a2", b"inner"], b"y");

    // One batch deletes a tree and moves the tree beside it.
    let ops = vec![felled(&[b"p"], b"b"), rename(&[b"p"], b"c", b"c2")];
    calls.batch("a delete and a move", ops);
    calls.refused(|calls| calls.get(&[b"p"], b"b"));
    calls.get(&[b"p", b"c2", b"inner", b"deeper"], b"z");
    let ops = vec![rename(&[b"p"], b"c2", b"c"), felled(&[b"p"], b"d")];
    calls.batch("a move and a delete", ops);
    calls.get(&[b"p", b"c", b"inner", b"deeper"], b"z");
    calls.refused(|calls| calls.get(&[b"p"], b"d"));
    // A tree goes to the key that a tree was deleted under, in a batch
    // that makes a tree at the key of the other deleted tree.
    let ops = vec![
        insert(&[b"p"], b"b", Element::empty_tree()),
        insert(&[b"p", b"b"], b"new", item(5)),
        rename(&[b"p"], b"c", b"d"),
    ];
    calls.batch("to the key of a deleted tree", ops);
    calls.get(&[b"p", b"b"], b"new");
    calls.get(&[b"p", b"d", b"inner"], b"y");
    calls.refused(|calls| calls.get(&[b"p", b"d"], b"new"));

    // A delete in one tree and a move in another, and a delete in the
    // tree beside the moved one.
    let ops = vec![felled(&[b"q"], b"e"), rename(&[b"p"], b"d", b"c")];
    calls.batch("a delete in another tree", ops);
    calls.refused(|calls| calls.get(&[b"q"], b"e"));
    let ops = vec![
        felled(&[b"p", b"a2"], b"inner"),
        rename(&[b"p"], b"c", b"c3"),
    ];
    calls.batch("a delete in the tree beside it", ops);
    calls.refused(|calls| calls.get(&[b"p", b"a2", b"inner"], b"y"));
    calls.get(&[b"p", b"c3", b"inner", b"deeper"], b"z");

    let ops = vec![felled(&[b"p"], b"b"), rename(&[b"p"], b"c3", b"b")];
    calls.refused(|calls| calls.batch("a delete at the key a move goes to", ops));
    let ops = vec![
        felled(&[b"p", b"c3"], b"inner"),
        rename(&[b"p"], b"c3", b"c"),
    ];
    calls.refused(|calls| calls.batch("a delete under the key a move leaves", ops));
    let ops = vec![felled(&[], b"p"), rename(&[b"p"], b"c3", b"c")];
    calls.refused(|calls| calls.batch("a delete of the tree a move is made in", ops));
    calls.get(&[b"p", b"c3", b"inner", b"deeper"], b"z");

    drop(calls);
    let transaction = db.start_transaction();
    let mut calls = Calls::new(&db, version, Some(&transaction), &mut log);
    calls.batch("a tree in a tree", vec![felled(&[b"q", b"f"], b"inner")]);
    calls.batch("the tree above it", vec![rename(&[b"q"], b"f", b"g")]);
    calls.get(&[b"q", b"g"], b"x");
    calls.refused(|calls| calls.get(&[b"q", b"g"], b"inner"));
    let ops = vec![felled(&[b"p"], b"b"), rename(&[b"p"], b"c3", b"c")];
    calls.batch("a delete and a move", ops);
    calls.get(&[b"p", b"c", b"inner", b"deeper"], b"z");
    calls.batch("moved again", vec![rename(&[b"p"], b"c", b"b")]);
    drop(calls);
    let committed = db.commit_transaction(transaction);
    log.answer("commit".to_owned(), committed);

    let mut calls = Calls::new(&db, version, None, &mut log);
    calls.root();
    calls.get(&[b"q", b"g"], b"x");
    calls.refused(|calls| calls.get(&[b"q", b"g"], b"inner"));
    calls.get(&[b"p", b"b", b"inner", b"deeper"], b"z");
    calls.refused(|calls| calls.get(&[b"p", b"b"], b"new"));
    calls.proven(&[b"p"]);
    calls.proven(&[b"q"]);
    calls.verify();
    db.flush().expect("a flush");
    log
}

/// A move in a batch that also deletes a tree with what it holds, and a
/// move after such a delete, answer and cost the same on both engines.
/// On LigninDB the deleted tree's subtrees leave the directory with the
/// batch that removes its element ([`super::super::gone`]), the moved
/// tree's subtrees go to their new prefixes with the same batch
/// ([`super::super::moved`]), and the physical checker finds the store
/// sound: no entry is left under a node that is gone.
#[test]
fn a_tree_is_moved_beside_the_delete_of_a_tree_on_grove_v4_and_v5() {
    for version in [&GROVE_VERSIONS[3], &GROVE_VERSIONS[4]] {
        on_both_engines!(moved_beside_deletes, version);
    }
}

/// Puts, at the seam, one node under `key` into the Merk of the tree at
/// `at` that the tree of the Merk does not reach: a change set with the
/// put and no root, as a storage batch that keeps a put over a later
/// delete of the same key hands a store one. RocksDB keeps the node as a
/// record under its key. LigninDB keeps it as an unlinked record of the
/// subtree (`transactions.md`, "Unlinked records"), and the write enters
/// the subtree where nothing was written into the tree before.
fn put_unlinked<E: Engine>(db: &GroveDb<E>, at: &[&[u8]], key: &[u8], v: &GroveVersion) {
    use grovedb_merk::{
        store::{NodeChange, NodeChangeSet, NodeStore},
        tree::TreeNode,
        TreeFeatureType,
    };

    let engine = db.engine();
    let tx = engine.start_transaction();
    let batch = E::Batch::default();
    let handle = engine.subtree(at.into(), Some(&batch), &tx).unwrap();
    let value = (Element::new_item(vec![6; 30]).serialize(v)).expect("the bytes of an element");
    let node = TreeNode::new(key.to_vec(), value, None, TreeFeatureType::BasicMerkNode).unwrap();
    let mut change_set = handle.change_set();
    let prepared = change_set.prepare_node(&node, NodeChange::ValueWritten);
    (change_set.put_node(key, prepared, None)).expect("the node is put");
    (handle.commit(change_set).unwrap()).expect("the change set is staged");
    drop(handle);
    (engine.apply_batch(batch, &tx).unwrap()).expect("a batch");
    (engine.commit_transaction(tx).unwrap()).expect("a commit");
}

/// Whether the store holds a node or a record under `key` for the path
/// `at`, read at the seam with no element asked for on the way.
fn holds<E: Engine>(db: &GroveDb<E>, at: &[&[u8]], key: &[u8]) -> bool {
    use grovedb_merk::store::NodeStore;

    let engine = db.engine();
    let tx = engine.start_read_transaction();
    let handle = engine.subtree(at.into(), None, &tx).unwrap();
    handle.has_node(key).unwrap().expect("a read of the store")
}

/// The trees of [`records_that_no_tree_links_at_a_move`] under the paths
/// they have before the moves, with whether the element of each names a
/// root, and under the paths they have afterwards.
const WITH_LOOSE_RECORDS: [(&[&[u8]], bool, &[&[u8]]); 6] = [
    (&[b"t"], true, &[b"u"]),
    (&[b"t", b"full"], true, &[b"u", b"full"]),
    (&[b"t", b"never"], false, &[b"u", b"never"]),
    (&[b"t", b"emptied"], false, &[b"u", b"emptied"]),
    (&[b"bare"], false, &[b"erab"]),
    (&[b"spent"], false, &[b"tneps"]),
];

/// Trees whose Merk holds a record that no tree links, and their moves
/// (D-267). The tree `t` holds an item, a tree `full` with an item, a
/// tree `never` that nothing else was written into, and a tree `emptied`
/// that was written into and emptied; `bare` and `spent` under the root
/// are trees of those last two kinds. Each of the six holds one such
/// record, put at the seam. `t` is moved, and then `bare` and `spent`
/// are, in one batch. GroveDB asks the engine to walk the trees whose
/// element names a root, which are `t` and `full`, and no other.
///
/// Then a tree `whole` with three elements changes places with a tree
/// `hollow` that was written into and emptied and holds such a record.
///
/// Gives the log of its calls up to the swap and the two reads after it,
/// with what the store holds of the six records under the paths the
/// trees have after the moves among them: read at the seam, and logged as
/// the answer with no cost. Apart from the log it gives what is read at
/// the end, where the two engines keep different records: whether each of
/// the six records is held under the path its tree had before and
/// whether one is held under each key of the swap, `whole` and then
/// `hollow`; and what GroveDB answers a `get` of the record's key in the
/// tree that the swap brought to `hollow`. `moved` is called between the
/// moves and the writes that follow them.
fn records_that_no_tree_links_at_a_move<E: Engine>(
    db: &GroveDb<E>,
    version: &'static GroveVersion,
    moved: impl FnOnce(&GroveDb<E>),
) -> (Log, Vec<bool>, Result<Element, Error>) {
    let mut log = Log::default();
    let mut calls = Calls::new(db, version, None, &mut log);
    let item = |byte: u8| Element::new_item(vec![byte; 14]);
    let ops = vec![
        insert(&[], b"t", Element::empty_tree()),
        insert(&[b"t"], b"x", item(1)),
        insert(&[b"t"], b"full", Element::empty_tree()),
        insert(&[b"t", b"full"], b"y", item(2)),
        insert(&[b"t"], b"never", Element::empty_tree()),
        insert(&[b"t"], b"emptied", Element::empty_tree()),
        insert(&[b"t", b"emptied"], b"gone", item(3)),
        insert(&[], b"bare", Element::empty_tree()),
        insert(&[], b"spent", Element::empty_tree()),
        insert(&[b"spent"], b"gone", item(4)),
        insert(&[], b"whole", Element::empty_tree()),
        insert(&[b"whole"], b"a", item(6)),
        insert(&[b"whole"], b"m", item(6)),
        insert(&[b"whole"], b"z", item(6)),
        insert(&[], b"hollow", Element::empty_tree()),
        insert(&[b"hollow"], b"gone", item(7)),
    ];
    calls.batch("made", ops);
    let ops = vec![
        delete(&[b"t", b"emptied"], b"gone"),
        delete(&[b"spent"], b"gone"),
        delete(&[b"hollow"], b"gone"),
    ];
    calls.batch("emptied", ops);
    let hollow: &[&[u8]] = &[b"hollow"];
    for tree in WITH_LOOSE_RECORDS
        .iter()
        .map(|(tree, ..)| *tree)
        .chain([hollow])
    {
        put_unlinked(db, tree, b"loose", version);
        assert!(holds(db, tree, b"loose"), "{tree:?}");
    }
    // No element says anything of the records.
    calls.root();
    calls.get(&[], b"t");
    calls.get(&[b"t"], b"never");
    calls.get(&[b"t"], b"emptied");
    calls.get(&[], b"bare");
    calls.get(&[], b"spent");

    calls.batch("the tree above", vec![rename(&[], b"t", b"u")]);
    let ops = vec![
        rename(&[], b"bare", b"erab"),
        rename(&[], b"spent", b"tneps"),
    ];
    calls.batch("the trees themselves", ops);
    for (.., tree) in WITH_LOOSE_RECORDS {
        let held = holds(db, tree, b"loose");
        let name = tree
            .iter()
            .fold(String::from("holds"), |mut name, segment| {
                name.push('/');
                name.push_str(&String::from_utf8_lossy(segment));
                name
            });
        let read = (name, OperationCost::default(), held.to_string());
        calls.log.answers.push(read);
    }
    moved(db);

    // Each tree is written under the path it has now.
    let written = |&(.., tree): &(&[&[u8]], bool, &[&[u8]])| insert(tree, b"new", item(5));
    let ops = WITH_LOOSE_RECORDS.iter().map(written).collect();
    calls.batch("written afterwards", ops);
    for (.., tree) in WITH_LOOSE_RECORDS {
        calls.get(tree, b"new");
    }
    calls.proven(&[b"u"]);
    calls.proven(&[b"erab"]);
    calls.verify();

    // The tree with elements changes places with the tree that has no
    // node and holds such a record.
    let ops = vec![
        rename(&[], b"whole", b"hollow"),
        rename(&[], b"hollow", b"whole"),
    ];
    calls.batch("a swap", ops);
    calls.get(hollow, b"m");
    calls.refused(|calls| calls.get(&[b"whole"], b"m"));
    let left = WITH_LOOSE_RECORDS.map(|(tree, ..)| holds(db, tree, b"loose"));
    let mut left = left.to_vec();
    // Under the key the tree with no node went to, and under the key it
    // left, where the tree with elements is now: by the store, and then
    // by a read of GroveDB. Merk looks for a key among the nodes it has
    // loaded, which after an open is the root, and asks the store for the
    // key where a link leads on from them: the key of the record is on
    // the side of the root where the tree has a node.
    left.push(holds(db, &[b"whole"], b"loose"));
    left.push(holds(db, hollow, b"loose"));
    let read = db.get(hollow, b"loose", None, version).unwrap();
    (log, left, read)
}

/// D-267, and row D12 of `transactions.md` 14.5. A record that no tree
/// links goes where the walk of a move takes its Merk, and nowhere
/// otherwise:
///
/// - **Under a tree whose element names a root** (the moved tree, and a
///   tree in it) the record is read under the new path and not under the
///   old one, on both engines, and the move is charged the same on both.
/// - **Under a tree whose element names no root**, nothing of the record
///   is read under the new path on either engine. On LigninDB the look
///   names no pair for such a tree. One below the moved tree leaves the
///   directory with the move, and one that is moved itself, to a key
///   that held no element, leaves it with the node of its element under
///   the old key ([`super::super::gone`]): no move is stated for it,
///   nothing is entered under either prefix afterwards, and its record is
///   gone. On RocksDB the copy walks no such tree, and the record stays
///   under the old prefix, which no element leads to: the difference
///   that is written down as D12.
/// - **Where a tree with elements changes places with such a tree**, the
///   subtree with no node makes room on LigninDB and its record goes
///   with it ([`super::super::moved`]). On RocksDB the record stays
///   under the prefix, the records of the tree with elements land beside
///   it, and `get` of GroveDB under the record's key answers with it
///   here, as an element of a tree that no write put it into: Merk asks
///   the store for a key where a link leads on from the nodes it has
///   loaded. That is D12 again, read under a path that an element leads
///   to.
///
/// Every call in the log that the two engines share answers and costs
/// the same, and the store is read to hold the same under the new paths
/// after the moves. The reads that the history gives apart are not in
/// that log: each engine is held to its own answer there, and the cost of
/// the `get` after the swap is not compared. The grove's own check, which
/// the history runs before the swap, finds nothing, and the physical
/// checker reads the LigninDB store.
#[test]
fn a_record_that_no_tree_links_goes_only_where_the_walk_of_a_move_takes_its_merk() {
    let entered = |db: &Lignin, at: &[&[u8]]| {
        let engine = db.engine();
        let tx = engine.start_read_transaction();
        let handle = engine.subtree(at.into(), None, &tx).unwrap();
        handle.entry().expect("the directory answers").is_some()
    };
    for version in [&GROVE_VERSIONS[3], &GROVE_VERSIONS[4]] {
        let dir = tempfile::tempdir().expect("a directory");
        let (log, left, read) = {
            let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
            let history = records_that_no_tree_links_at_a_move(&db, version, |db| {
                for (before, rooted, after) in WITH_LOOSE_RECORDS {
                    assert!(!entered(db, before), "{before:?}");
                    assert_eq!(entered(db, after), rooted, "{after:?}");
                }
            });
            // After the swap the subtree of the tree with elements is
            // entered under the key it went to, and nothing is entered
            // under the key it left.
            assert!(entered(&db, &[b"hollow"]));
            assert!(!entered(&db, &[b"whole"]));
            db.flush().expect("a flush");
            history
        };
        log.assert_refusals();
        check(dir.path(), version);
        let held: Vec<bool> = (log.answers.iter())
            .filter(|(call, ..)| call.starts_with("holds"))
            .map(|(.., held)| held == "true")
            .collect();
        let rooted = WITH_LOOSE_RECORDS.map(|(_, rooted, _)| rooted);
        assert_eq!(held, rooted);
        assert_eq!(left, [false; 8]);
        assert!(matches!(read, Err(Error::PathKeyNotFound(_))), "{read:?}");
        #[cfg(feature = "rocksdb_storage")]
        {
            let dir = tempfile::tempdir().expect("a directory");
            let db = GroveDb::<crate::RocksDbEngine>::open_with_engine(dir.path())
                .expect("a database on RocksDB");
            let (on_rocksdb, left, read) =
                records_that_no_tree_links_at_a_move(&db, version, |_| {});
            assert_same(&log, &on_rocksdb);
            // The copy cleared the prefixes it walked, and left the
            // records of the trees it did not walk: under the paths
            // that no element leads to any more, and under the prefix
            // of `hollow`, where the records of the tree with elements
            // landed beside the one that was there. GroveDB reads that
            // one as an element of the tree.
            let mut kept = rooted.map(|rooted| !rooted).to_vec();
            kept.extend([false, true]);
            assert_eq!(left, kept);
            let read = read.expect("the record is read as an element");
            assert_eq!(read, Element::new_item(vec![6; 30]));
        }
    }
}

macro_rules! moved_with_their_data {
    ($($(#[$attribute:meta])* $test:ident: $history:ident, $kind:ty, $first:expr;)*) => {$(
        #[test]
        $(#[$attribute])*
        fn $test() {
            moved_alike!($history::<$kind, _>, $first);
        }
    )*};
}

moved_with_their_data! {
    an_mmr_tree_in_a_moved_tree_goes_with_it_on_both_engines: flat_trees_below, Mmr, 4;
    a_bulk_append_tree_in_a_moved_tree_goes_with_it_on_both_engines: flat_trees_below, Bulk, 4;
    a_dense_tree_in_a_moved_tree_goes_with_it_on_both_engines: flat_trees_below, Dense, 4;
    a_commitment_tree_in_a_moved_tree_goes_with_it_on_both_engines:
        flat_trees_below, Commitment, 4;
    a_private_document_store_in_a_moved_tree_goes_with_it_on_both_engines:
        flat_trees_below, Private, 4;
    an_mmr_tree_is_moved_with_its_leaves_on_both_engines: flat_trees, Mmr, 4;
    a_bulk_append_tree_is_moved_with_its_values_on_both_engines: flat_trees, Bulk, 4;
    a_dense_tree_is_moved_with_its_values_on_both_engines: flat_trees, Dense, 4;
    a_commitment_tree_is_moved_with_its_notes_on_both_engines: flat_trees, Commitment, 4;
    a_private_document_store_is_moved_with_its_entries_on_both_engines: flat_trees, Private, 4;
    a_count_indexed_tree_is_moved_with_its_axes_on_both_engines: indexed_trees, Count, 4;
    a_sum_indexed_tree_is_moved_with_its_axes_on_both_engines: indexed_trees, Sum, 4;
    a_count_and_sum_indexed_tree_is_moved_with_its_axes_on_both_engines: indexed_trees, CountSum, 4;
    an_indexed_tree_with_two_of_three_axes_is_moved_with_them_on_both_engines:
        indexed_trees, SumAndAverage, 4;
    a_big_sum_indexed_tree_is_moved_with_its_axes_on_both_engines: indexed_trees, BigSum, 5;
    a_count_and_big_sum_indexed_tree_is_moved_with_its_axes_on_both_engines:
        indexed_trees, CountBigSum, 5;
}

#[test]
fn trees_are_moved_on_grove_v4() {
    moved_trees_on(&GROVE_VERSIONS[3]);
}

#[test]
fn trees_are_moved_on_grove_v5() {
    moved_trees_on(&GROVE_VERSIONS[4]);
}
