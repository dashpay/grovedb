//! A tree of one kind at a path where a tree of another kind stood, and
//! the data keyspace of its subtree by the tree type of its entry
//! (`lignindb::verify`, "The data column by the tree type of the entry").
//!
//! A tree whose data is a Merk and a tree that keeps flat records leave
//! records of different kinds under one prefix. Two operations of GroveDB
//! put a tree at a path where another stood with what it held: a batch
//! that writes an element over the tree, and an insert at the path of a
//! tree that was dropped flat. The engine drops the subtree of the tree
//! that went (`engine.md`, "An element written over a tree"), and RocksDB
//! keeps its records, so the answers of the two engines after such a
//! write are not compared here. These tests hold LigninDB alone: where
//! the history is taken, the element at the path is one of the new
//! tree's kind, the items and the row that were written into a new
//! Merk are read back, a new tree that keeps flat records counts its two
//! values (`Kind::read_back`), and the check of the database, which the engine
//! gives the tree types that keep flat records, finds nothing when the
//! database is closed and when it is opened again.

use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

use super::{
    append_only::{check, Appended, Bulk, Commitment, Dense, Mmr},
    indexed::{Indexed, Sum},
    Lignin,
};
use crate::{batch::QualifiedGroveDbOp, BackwardsReferences, Element};

const ROOT: &[&[u8]] = &[];
const TREE: &[u8] = b"t";

/// A kind of tree, with what a history writes into it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// A normal tree, with an item, and a tree that holds an item.
    Plain,
    /// A sum-indexed tree with one row.
    Indexed,
    /// A tree that keeps flat records, with two values, and on grove
    /// versions 1 to 3 a keyed child that a batch inserts under it.
    Mmr,
    Bulk,
    Dense,
    Commitment,
}

impl Kind {
    const ALL: [Self; 6] = [
        Self::Plain,
        Self::Indexed,
        Self::Mmr,
        Self::Bulk,
        Self::Dense,
        Self::Commitment,
    ];

    fn empty(self) -> Element {
        match self {
            Self::Plain => Element::empty_tree(),
            Self::Indexed => Sum::empty(),
            Self::Mmr => Mmr::empty(None),
            Self::Bulk => Bulk::empty(None),
            Self::Dense => Dense::empty(None),
            Self::Commitment => Commitment::empty(None),
        }
    }

    /// Writes into the tree under [`TREE`] what a tree of the kind holds,
    /// on the grove version `number`, and says whether every write was
    /// taken.
    fn fill(self, db: &Lignin, v: &GroveVersion, number: usize) -> bool {
        let tree: &[&[u8]] = &[TREE];
        let item = || Element::new_item(b"an item".to_vec());
        match self {
            Self::Plain => {
                let below: &[&[u8]] = &[TREE, b"child"];
                let child = Element::empty_tree();
                let taken = [
                    db.insert(tree, b"item", item(), None, None, v).unwrap(),
                    db.insert(tree, b"child", child, None, None, v).unwrap(),
                    db.insert(below, b"item", item(), None, None, v).unwrap(),
                ];
                taken.iter().all(Result::is_ok)
            }
            Self::Indexed => {
                (Sum::insert(db, tree, b"child", Sum::leaf(4), None, v).unwrap()).is_ok()
            }
            flat => {
                let append = |n: usize| match flat {
                    Self::Mmr => Mmr::append(db, ROOT, TREE, n, None, v).unwrap().is_ok(),
                    Self::Bulk => Bulk::append(db, ROOT, TREE, n, None, v).unwrap().is_ok(),
                    Self::Dense => Dense::append(db, ROOT, TREE, n, None, v).unwrap().is_ok(),
                    _ => Commitment::append(db, ROOT, TREE, n, None, v)
                        .unwrap()
                        .is_ok(),
                };
                let appended = append(0) && append(1);
                if !appended || number > 3 {
                    return appended;
                }
                let child = QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TREE.to_vec()],
                    b"child".to_vec(),
                    item(),
                );
                db.apply_batch(vec![child], None, None, v).unwrap().is_ok()
            }
        }
    }
}

impl Kind {
    /// Reads the tree under [`TREE`] back after [`Self::fill`]. Its
    /// element is one of the kind. A normal tree has its item and the
    /// item of the tree in it, by value, and an indexed tree its row, by
    /// value. A tree that keeps flat records answers that it has two
    /// values, and its element is no longer that of an empty tree. The
    /// values of such a tree are not read, and neither is its keyed
    /// child.
    fn read_back(self, db: &Lignin, v: &GroveVersion, told: &str) {
        let tree: &[&[u8]] = &[TREE];
        let element = db.get(ROOT, TREE, None, v).unwrap().expect("the tree");
        let kind = std::mem::discriminant(&element);
        assert_eq!(
            kind,
            std::mem::discriminant(&self.empty()),
            "{told}: {element:?}"
        );
        let item = Element::new_item(b"an item".to_vec());
        match self {
            Self::Plain => {
                let below: &[&[u8]] = &[TREE, b"child"];
                let read = db.get(tree, b"item", None, v).unwrap();
                assert_eq!(read.expect("the item"), item, "{told}");
                let read = db.get(below, b"item", None, v).unwrap();
                assert_eq!(read.expect("the item below"), item, "{told}");
            }
            Self::Indexed => {
                let read = db.get(tree, b"child", None, v).unwrap();
                assert_eq!(read.expect("the row"), Sum::leaf(4), "{told}");
            }
            flat => {
                let count = match flat {
                    Self::Mmr => db.mmr_tree_leaf_count(ROOT, TREE, None, v).unwrap(),
                    Self::Bulk => db.bulk_count(ROOT, TREE, None, v).unwrap(),
                    Self::Dense => {
                        (db.dense_tree_count(ROOT, TREE, None, v).unwrap()).map(u64::from)
                    }
                    _ => db.commitment_tree_count(ROOT, TREE, None, v).unwrap(),
                };
                assert_eq!(count.expect("the count"), 2, "{told}");
                assert_ne!(element, self.empty(), "{told}");
            }
        }
    }
}

/// How a tree comes to stand where another stood.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Way {
    /// A batch writes its element over the tree.
    Over,
    /// The tree is dropped flat, and the new one is inserted at its path
    /// before any drain.
    AfterFlatDrop,
}

/// A tree of the kind `old` with what it holds, a tree of the kind `new`
/// put in its place by `way`, and what a tree of that kind holds written
/// into the new one. Returns whether all of it was taken. The database is
/// closed, which checks it in a debug build, and is checked once more
/// after it is opened again, whatever was taken.
fn replaced(v: &GroveVersion, number: usize, old: Kind, new: Kind, way: Way) -> bool {
    let dir = tempfile::tempdir().expect("a directory");
    let taken = {
        let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
        let made = (db.insert(ROOT, TREE, old.empty(), None, None, v).unwrap()).is_ok();
        let put = made
            && old.fill(&db, v, number)
            && match way {
                Way::Over => {
                    let over = QualifiedGroveDbOp::insert_or_replace_op(
                        vec![],
                        TREE.to_vec(),
                        new.empty(),
                    );
                    db.apply_batch(vec![over], None, None, v).unwrap().is_ok()
                }
                Way::AfterFlatDrop => {
                    let unchecked = BackwardsReferences::DontCheck;
                    (db.drop_flat_subtree(ROOT, TREE, unchecked, None, v)
                        .unwrap())
                    .is_ok()
                        && (db.insert(ROOT, TREE, new.empty(), None, None, v).unwrap()).is_ok()
                }
            };
        let taken = put && new.fill(&db, v, number);
        if taken {
            new.read_back(
                &db,
                v,
                &format!("{old:?} then {new:?} on grove version {number}"),
            );
        }
        db.flush().expect("a flush");
        taken
    };
    check(dir.path(), v);
    taken
}

/// Every pair of kinds of `way` on the grove version `number`: the pairs
/// whose history was taken whole.
fn reached(v: &GroveVersion, number: usize, way: Way) -> Vec<(Kind, Kind)> {
    let mut reached = Vec::new();
    for old in Kind::ALL {
        for new in Kind::ALL {
            if replaced(v, number, old, new, way) {
                reached.push((old, new));
            }
        }
    }
    println!(
        "grove version {number}, {way:?}: {} of {} pairs taken: {reached:?}",
        reached.len(),
        Kind::ALL.len() * Kind::ALL.len()
    );
    reached
}

/// The pairs in which a tree whose data is a Merk and a tree that keeps
/// flat records follow each other, and two that both keep flat records.
fn across() -> Vec<(Kind, Kind)> {
    let flat = [Kind::Mmr, Kind::Bulk, Kind::Dense, Kind::Commitment];
    let mut pairs = vec![(Kind::Mmr, Kind::Bulk), (Kind::Dense, Kind::Mmr)];
    for kind in flat {
        pairs.extend([
            (Kind::Plain, kind),
            (kind, Kind::Plain),
            (Kind::Indexed, kind),
        ]);
    }
    pairs
}

/// A batch writes a tree over a tree of every other kind that holds data,
/// on every grove version, and the new tree is written into: the store is
/// sound at each close, whichever of the histories the engine takes. The
/// histories in which a Merk and flat records follow each other under one
/// prefix are taken on every grove version.
#[test]
fn a_tree_written_over_a_tree_of_another_kind_leaves_a_sound_store() {
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        let reached = reached(v, number, Way::Over);
        for pair in across() {
            assert!(
                reached.contains(&pair),
                "{pair:?} on grove version {number}"
            );
        }
    }
}

/// A tree is dropped flat with what it holds, and a tree of every kind is
/// made again at its path and written into, before any drain: the store
/// is sound at each close, on every grove version. Where the grove
/// version has the flat drop, the histories in which a Merk and flat
/// records follow each other under one prefix are taken.
#[test]
fn a_tree_made_again_where_one_was_dropped_flat_leaves_a_sound_store() {
    let mut versions_with_the_drop = 0;
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        let reached = reached(v, number, Way::AfterFlatDrop);
        if reached.is_empty() {
            continue;
        }
        versions_with_the_drop += 1;
        for pair in across() {
            assert!(
                reached.contains(&pair),
                "{pair:?} on grove version {number}"
            );
        }
    }
    assert!(
        versions_with_the_drop > 0,
        "no grove version has the flat drop"
    );
}
