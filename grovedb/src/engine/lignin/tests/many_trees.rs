//! State sync of a grove whose one tree holds thousands of trees
//! (GROVE-10, D-340): the grove of the gate's state sync run.
//!
//! The groves of `sync` have 23 rows and 7 children, and a tree of theirs
//! is named in its host with room in the page. Here one tree holds
//! [`TREES`] trees of five shapes, written by batches, and all but one in
//! [`UNTOUCHED`] of them hold a Merk, so that a restore names tree after
//! tree in pages of their host that were laid out full. The grove is
//! synced from each engine into each engine, and what the syncs leave is
//! compared record for record with both sources; the synced groves then
//! take the same writes as the sources and end on the same root hash.

use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

use super::{
    queries::path,
    sync::{begin, open, records, Requests, Shape, Side, PARTS},
    LigninEngine,
};
use crate::{
    batch::QualifiedGroveDbOp, engine::Engine, replication::RestoreCommitMode, Element, GroveDb,
};

/// How many trees the tree at [`MANY`] holds.
const TREES: u16 = 3_000;

/// How many of those trees one batch of the fixture makes.
const IN_A_BATCH: usize = 250;

/// How many shapes the trees have: the tree `n` has the shape `n % SHAPES`.
const SHAPES: u16 = 5;

/// One tree in so many is a tree that nothing is written into.
const UNTOUCHED: u16 = 100;

const MANY: &[u8] = b"many";
const BESIDE: &[u8] = b"beside";
const IN: &[u8] = b"in";

fn key(n: u16) -> Vec<u8> {
    n.to_be_bytes().to_vec()
}

fn op(at: &[&[u8]], key: &[u8], element: Element) -> QualifiedGroveDbOp {
    QualifiedGroveDbOp::insert_or_replace_op(path(at), key.to_vec(), element)
}

/// The operations that make the tree `n` under [`MANY`], with the paths of
/// the Merks they make: a tree with two items, a tree with one item, a
/// sum tree with two sum items, a tree whose one element is a tree with
/// an item, which is the root of its Merk, and a tree with five items of
/// 60 bytes. One tree in [`UNTOUCHED`] is a tree of the first shape that
/// nothing is written into.
fn tree(n: u16, merks: &mut Vec<(Vec<Vec<u8>>, bool)>) -> Vec<QualifiedGroveDbOp> {
    let name = key(n);
    let at: &[&[u8]] = &[MANY, name.as_slice()];
    let item = |byte: u8, len: usize| Element::new_item(vec![byte; len]);
    let sums = n % SHAPES == 2;
    merks.push((path(at), sums));
    let host = match sums {
        true => Element::empty_sum_tree(),
        false => Element::empty_tree(),
    };
    let mut ops = vec![op(&[MANY], &name, host)];
    match n % SHAPES {
        0 if n.is_multiple_of(UNTOUCHED) => {}
        0 => {
            ops.push(op(at, b"k", item(0, 8)));
            ops.push(op(at, b"l", item(0, 8)));
        }
        1 => ops.push(op(at, b"k", item(1, 8))),
        2 => {
            ops.push(op(at, b"a", Element::new_sum_item(i64::from(n))));
            ops.push(op(at, b"b", Element::new_sum_item(-7)));
        }
        3 => {
            ops.push(op(at, IN, Element::empty_tree()));
            let inner: &[&[u8]] = &[MANY, name.as_slice(), IN];
            ops.push(op(inner, b"k", item(3, 8)));
            merks.push((path(inner), false));
        }
        _ => ops.extend((0..5u8).map(|row| op(at, &[row], item(row, 60)))),
    }
    ops
}

fn batch<E: Engine>(db: &GroveDb<E>, ops: Vec<QualifiedGroveDbOp>, v: &GroveVersion) {
    let applied = db.apply_batch(ops, None, None, v);
    applied.unwrap().expect("the batch is taken");
}

/// Makes the grove in `db`: under the root an item, a tree with an item
/// and the tree at [`MANY`] with its [`TREES`] trees, made by batches of
/// [`IN_A_BATCH`] trees. Returns the path of every Merk of it.
fn grove<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Shape {
    let mut merks = vec![
        (Vec::new(), false),
        (path(&[BESIDE]), false),
        (path(&[MANY]), false),
    ];
    let first = vec![
        op(&[], b"item", Element::new_item(vec![9; 20])),
        op(&[], BESIDE, Element::empty_tree()),
        op(&[BESIDE], b"k", Element::new_item(vec![8; 20])),
        op(&[], MANY, Element::empty_tree()),
    ];
    batch(db, first, v);
    let trees: Vec<u16> = (0..TREES).collect();
    for trees in trees.chunks(IN_A_BATCH) {
        let ops = trees.iter().flat_map(|n| tree(*n, &mut merks)).collect();
        batch(db, ops, v);
    }
    Shape {
        merks,
        ..Shape::default()
    }
}

/// The writes a grove takes after it was synced, with the root hash they
/// leave: at the start, the middle and the end of [`MANY`], the one item
/// of a tree deleted, which leaves the tree with nothing, and then a
/// write into two trees of every shape, that tree among them, and at the
/// start and in the middle a tree that nothing was written into; a tree
/// made under [`MANY`]; and one batch that writes into two hundred of
/// the trees.
fn written<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> [u8; 32] {
    let put = |at: &[&[u8]], key: &[u8], element: Element| {
        let inserted = db.insert(at, key, element, None, None, v);
        inserted.unwrap().expect("the element is taken");
    };
    for start in [0, TREES / 2, TREES - 2 * SHAPES] {
        let emptied = key(start + 1);
        let at: &[&[u8]] = &[MANY, emptied.as_slice()];
        let deleted = db.delete(at, b"k", None, None, v);
        deleted.unwrap().expect("the item is deleted");
        let emptied = db
            .is_empty_tree(at, None, v)
            .unwrap()
            .expect("the tree is read");
        assert!(emptied, "the tree {} holds nothing", start + 1);
        for n in start..start + 2 * SHAPES {
            let name = key(n);
            let at: &[&[u8]] = &[MANY, name.as_slice()];
            let inner: &[&[u8]] = &[MANY, name.as_slice(), IN];
            match n % SHAPES {
                2 => put(at, b"c", Element::new_sum_item(11)),
                3 => put(inner, b"more", Element::new_item(vec![4; 8])),
                _ => put(at, b"more", Element::new_item(vec![5; 8])),
            }
        }
    }
    put(&[MANY], b"a tree made after", Element::empty_tree());
    put(
        &[MANY, b"a tree made after"],
        b"k",
        Element::new_item(vec![6; 8]),
    );
    let ops = (0..200).map(|n| {
        let name = key(100 + n * 13);
        let element = match (100 + n * 13) % SHAPES {
            2 => Element::new_sum_item(3),
            _ => Element::new_item(vec![2; 30]),
        };
        op(&[MANY, name.as_slice()], b"by a batch", element)
    });
    batch(db, ops.collect(), v);
    db.root_hash(None, v).unwrap().expect("a root hash")
}

/// What a sync left.
#[derive(Debug, PartialEq)]
struct Synced {
    /// How many requests the session made.
    requests: usize,
    /// Every record of every Merk of the synced grove.
    records: Vec<String>,
    /// The root hash of the synced grove after the writes of [`written`].
    after: [u8; 32],
}

/// Syncs a new database on `T` from `source` and commits the session:
/// the synced grove has the root hash of its source and its own
/// verification finds no issue, before the writes of [`written`] and
/// after them. Before those writes and after them the database is closed
/// and [`Side::check`] is called: on LigninDB the physical checker reads
/// the closed database, and of a RocksDB database it checks nothing.
/// With how many times the session committed before its end.
fn synced<S: Engine, T: Side>(
    source: &GroveDb<S>,
    shape: &Shape,
    mode: RestoreCommitMode,
    v: &GroveVersion,
) -> (Synced, usize) {
    let dir = tempfile::tempdir().expect("a directory");
    let target = open::<T>(dir.path());
    let root = source.root_hash(None, v).unwrap().expect("a root hash");
    let mut session = begin(&target, root, mode, v).expect("a session starts");
    let mut requests = Requests::new(source, root, v);
    let mut served = 0;
    while let Some((request, payload)) = requests.fetch() {
        let applied = requests.apply(&mut session, &request, &payload);
        applied.expect("the chunk is taken");
        served += 1;
    }
    assert!(session.is_sync_completed());
    let commits = session.intermediate_commits();
    (target.commit_session(session, v)).expect("the session commits");
    let restored = target.root_hash(None, v).unwrap().expect("a root hash");
    assert_eq!(restored, root, "into {}", T::NAME);
    let issues = target.verify_grovedb(None, true, false, v);
    let issues = issues.expect("the verification ends");
    assert!(
        issues.is_empty(),
        "into {}: {} issues",
        T::NAME,
        issues.len()
    );
    let records = records(&target, shape);
    target.flush().expect("a flush");
    drop(target);
    T::check(dir.path(), v);

    let target = open::<T>(dir.path());
    let after = written(&target, v);
    let issues = target.verify_grovedb(None, true, false, v);
    assert!(issues.expect("the verification ends").is_empty());
    target.flush().expect("a flush");
    drop(target);
    T::check(dir.path(), v);
    let left = Synced {
        requests: served,
        records,
        after,
    };
    (left, commits)
}

/// The grove made on `E`, in a directory of its own.
fn made<E: Engine>(v: &GroveVersion) -> (GroveDb<E>, tempfile::TempDir, Shape) {
    let dir = tempfile::tempdir().expect("a directory");
    let db = open::<E>(dir.path());
    let shape = grove(&db, v);
    (db, dir, shape)
}

/// How many Merks the grove has: the root's, the two under the root, one
/// for each of the trees and one more for each tree of the fourth shape.
fn merks() -> usize {
    let two_deep = (0..TREES).filter(|n| n % SHAPES == 3).count();
    3 + usize::from(TREES) + two_deep
}

/// LigninDB is synced from LigninDB on every grove version: the synced
/// grove holds the records of its source, takes the writes its source
/// takes and ends on the root hash of its source after them. This is the
/// run of a build with LigninDB as its only engine.
#[test]
fn a_tree_of_thousands_of_trees_syncs_on_lignindb() {
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        let (source, dir, shape) = made::<LigninEngine>(v);
        assert_eq!(shape.merks.len(), merks());
        let once = RestoreCommitMode::Atomic;
        let (left, commits) = synced::<_, LigninEngine>(&source, &shape, once, v);
        assert_eq!(commits, 0, "grove version {number}");
        assert_eq!(
            left.records,
            records(&source, &shape),
            "grove version {number}"
        );
        assert!(left.records.len() > 3 * usize::from(TREES));
        assert_eq!(left.after, written(&source, v), "grove version {number}");
        source.flush().expect("a flush");
        drop(source);
        LigninEngine::check(dir.path(), v);
        println!(
            "grove version {number}: {} Merks, {} records, {} requests",
            shape.merks.len(),
            left.records.len(),
            left.requests
        );
    }
}

/// The grove is synced from each engine into each engine on every grove
/// version. The two sources have one root hash; the four synced groves
/// hold the records of both sources, in as many requests, and after the
/// writes of [`written`] the four and the two sources have one root hash.
#[cfg(feature = "rocksdb_storage")]
#[test]
fn a_tree_of_thousands_of_trees_syncs_between_the_engines_in_all_four_directions() {
    type Rocks = crate::RocksDbEngine;
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        let (rocks, _rocks_dir, shape) = made::<Rocks>(v);
        let (lignin, lignin_dir, same) = made::<LigninEngine>(v);
        assert_eq!(shape, same);
        assert_eq!(shape.merks.len(), merks());
        let on_rocks = rocks.root_hash(None, v).unwrap().expect("a root hash");
        let on_lignin = lignin.root_hash(None, v).unwrap().expect("a root hash");
        assert_eq!(on_rocks, on_lignin, "grove version {number}");

        let once = RestoreCommitMode::Atomic;
        let (first, none) = synced::<_, Rocks>(&rocks, &shape, once, v);
        assert_eq!(none, 0);
        let others = [
            synced::<_, LigninEngine>(&rocks, &shape, once, v),
            synced::<_, Rocks>(&lignin, &shape, once, v),
            synced::<_, LigninEngine>(&lignin, &shape, once, v),
        ];
        for (direction, (other, commits)) in others.iter().enumerate() {
            let told = format!("direction {direction} on grove version {number}");
            assert_eq!(*commits, 0, "{told}");
            assert_eq!(first.requests, other.requests, "{told}");
            assert!(first.records == other.records, "{told}: the records differ");
            assert_eq!(first.after, other.after, "{told}");
        }
        assert!(
            first.records == records(&rocks, &shape),
            "grove version {number}"
        );
        assert!(
            first.records == records(&lignin, &shape),
            "grove version {number}"
        );
        assert!(first.records.len() > 3 * usize::from(TREES));
        assert_eq!(first.after, written(&rocks, v), "grove version {number}");
        assert_eq!(first.after, written(&lignin, v), "grove version {number}");
        lignin.flush().expect("a flush");
        drop(lignin);
        LigninEngine::check(lignin_dir.path(), v);
        println!(
            "grove version {number}: {} Merks, {} records, {} requests in each of four directions",
            shape.merks.len(),
            first.records.len(),
            first.requests
        );
    }
}

/// A session that commits in parts, with a budget of one byte and one
/// subtree in flight, between the engines in both directions on the last
/// grove version: it commits more than a thousand times before its end
/// and leaves what a session that commits once leaves, in more requests.
#[cfg(feature = "rocksdb_storage")]
#[test]
fn a_tree_of_thousands_of_trees_syncs_between_the_engines_by_a_session_that_commits_in_parts() {
    type Rocks = crate::RocksDbEngine;
    let v = GROVE_VERSIONS.last().expect("a grove version");
    let (rocks, _rocks_dir, shape) = made::<Rocks>(v);
    let (lignin, _lignin_dir, _) = made::<LigninEngine>(v);
    let once = RestoreCommitMode::Atomic;
    let (at_once, _) = synced::<_, LigninEngine>(&rocks, &shape, once, v);
    let parts = [
        synced::<_, LigninEngine>(&rocks, &shape, PARTS, v),
        synced::<_, Rocks>(&lignin, &shape, PARTS, v),
    ];
    for (direction, (in_parts, commits)) in parts.iter().enumerate() {
        assert!(*commits > 1000, "direction {direction}: {commits} commits");
        let requests = in_parts.requests;
        assert!(
            requests > at_once.requests,
            "direction {direction}: {requests} requests"
        );
        assert!(in_parts.records == at_once.records, "direction {direction}");
        assert_eq!(in_parts.after, at_once.after, "direction {direction}");
        println!("direction {direction}: {commits} commits, {requests} requests");
    }
}
