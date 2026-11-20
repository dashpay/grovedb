//! Root-hash propagation through five levels of trees (GROVE-02): the same
//! calls on LigninDB and on RocksDB, with every answer, every
//! `OperationCost` and the root hash after every write compared.
//!
//! A chain is five trees one inside the other under the root. The kinds of
//! tree are those whose element hosts one Merk, which are the trees that
//! can hold a tree. On each grove version the chains cover every ordered
//! pair of the kinds that version makes, as a tree and the tree in it.

use std::collections::BTreeSet;

use grovedb_costs::{CostResult, OperationCost};
use grovedb_merk::{
    element::tree_type::ElementTreeTypeExtensions, store::lignin::hash::MerkHashRules,
};
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};
use lignindb::{verify::Scope, Db, Options};

use super::Lignin;
use crate::{
    engine::Engine, reference_path::ReferencePathType, Element, Error, GroveDb, TransactionArg,
};

/// How many trees a chain has, one inside the other.
const DEPTH: usize = 5;

/// A kind of tree whose element hosts one Merk.
struct Kind {
    name: &'static str,
    make: fn() -> Element,
    /// Whether the tree takes sum items.
    sums: bool,
}

/// The kinds of tree every grove version makes.
const NARROW: usize = 9;

/// Every kind, the kinds that only `GROVE_V5` makes last.
const KINDS: [Kind; 12] = [
    Kind {
        name: "tree",
        make: Element::empty_tree,
        sums: false,
    },
    Kind {
        name: "sum tree",
        make: Element::empty_sum_tree,
        sums: true,
    },
    Kind {
        name: "big sum tree",
        make: Element::empty_big_sum_tree,
        sums: true,
    },
    Kind {
        name: "count tree",
        make: Element::empty_count_tree,
        sums: false,
    },
    Kind {
        name: "count sum tree",
        make: Element::empty_count_sum_tree,
        sums: true,
    },
    Kind {
        name: "provable count tree",
        make: Element::empty_provable_count_tree,
        sums: false,
    },
    Kind {
        name: "provable count sum tree",
        make: Element::empty_provable_count_sum_tree,
        sums: true,
    },
    Kind {
        name: "provable sum tree",
        make: Element::empty_provable_sum_tree,
        sums: true,
    },
    Kind {
        name: "provable count provable sum tree",
        make: Element::empty_provable_count_provable_sum_tree,
        sums: true,
    },
    Kind {
        name: "provable count provable big sum tree",
        make: Element::empty_provable_count_provable_big_sum_tree,
        sums: true,
    },
    Kind {
        name: "count big sum tree",
        make: Element::empty_count_big_sum_tree,
        sums: true,
    },
    Kind {
        name: "provable big sum tree",
        make: Element::empty_provable_big_sum_tree,
        sums: true,
    },
];

/// One call of a history: what it was, what it was charged, and what it
/// returned or how it failed, as text.
type Answer = (String, OperationCost, String);

/// The calls of a history on one database, each with its answer.
struct Recorder<'db, 'a, E: Engine> {
    db: &'db GroveDb<E>,
    version: &'static GroveVersion,
    through: TransactionArg<'db, 'a, E>,
    answers: Vec<Answer>,
    /// How many of the calls were refused.
    refused: usize,
}

fn segments(path: &[Vec<u8>]) -> Vec<&[u8]> {
    path.iter().map(Vec::as_slice).collect()
}

fn named(call: &str, path: &[Vec<u8>], key: &[u8]) -> String {
    let mut name = String::from(call);
    for segment in path.iter().map(Vec::as_slice).chain([key]) {
        name.push('/');
        name.push_str(&String::from_utf8_lossy(segment));
    }
    name
}

impl<'db, 'a, E: Engine> Recorder<'db, 'a, E> {
    fn new(
        db: &'db GroveDb<E>,
        version: &'static GroveVersion,
        through: TransactionArg<'db, 'a, E>,
    ) -> Self {
        Self {
            db,
            version,
            through,
            answers: Vec::new(),
            refused: 0,
        }
    }

    fn answer<T: std::fmt::Debug>(&mut self, name: String, answer: CostResult<T, Error>) {
        let cost = answer.cost.clone();
        let value = answer.unwrap();
        self.refused += usize::from(value.is_err());
        self.answers.push((name, cost, format!("{value:?}")));
    }

    /// The root hash as the history's transaction reads it.
    fn root(&mut self) {
        let root = self.db.root_hash(self.through, self.version);
        self.answer("root".to_owned(), root);
    }

    fn insert(&mut self, path: &[Vec<u8>], key: &[u8], element: Element) {
        let inserted = self.db.insert(
            segments(path).as_slice(),
            key,
            element,
            None,
            self.through,
            self.version,
        );
        self.answer(named("insert", path, key), inserted);
        self.root();
    }

    fn delete(&mut self, path: &[Vec<u8>], key: &[u8]) {
        let deleted = self.db.delete(
            segments(path).as_slice(),
            key,
            None,
            self.through,
            self.version,
        );
        self.answer(named("delete", path, key), deleted);
        self.root();
    }

    fn get(&mut self, path: &[Vec<u8>], key: &[u8]) {
        let read = self
            .db
            .get(segments(path).as_slice(), key, self.through, self.version);
        self.answer(named("get", path, key), read);
    }

    fn is_empty(&mut self, path: &[Vec<u8>]) {
        let empty = self
            .db
            .is_empty_tree(segments(path).as_slice(), self.through, self.version);
        self.answer(named("empty", path, b""), empty);
    }
}

/// A number as a tree stores it: a sum item where the tree takes sums, and
/// an item otherwise.
fn number(sums: bool, number: i64) -> Element {
    match sums {
        true => Element::new_sum_item(number),
        false => Element::new_item(number.to_be_bytes().to_vec()),
    }
}

/// A chain under the key `top` of the root: five trees of the `kinds`
/// given, one inside the other, and then writes at every level of it, the
/// deepest first. Returns how many of its calls are refused.
fn chain<E: Engine>(
    recorder: &mut Recorder<'_, '_, E>,
    top: &[u8],
    kinds: [usize; DEPTH],
) -> usize {
    let mut path: Vec<Vec<u8>> = Vec::new();
    for (level, kind) in kinds.iter().enumerate() {
        let key = match level {
            0 => top.to_vec(),
            level => format!("level {}", level + 1).into_bytes(),
        };
        recorder.insert(&path, &key, (KINDS[*kind].make)());
        path.push(key);
    }
    let deep = path.clone();
    let deepest = &KINDS[kinds[DEPTH - 1]];
    let beside = [top, b" beside"].concat();
    let mut refused = 0;

    // The deepest tree is written into: each write changes the root hash
    // of five trees and of the root.
    recorder.insert(&deep, b"item", Element::new_item(vec![kinds[0] as u8; 9]));
    recorder.insert(&deep, b"long", Element::new_item(vec![7; 300]));
    recorder.insert(&deep, b"plus", number(deepest.sums, 41));
    recorder.insert(&deep, b"minus", number(deepest.sums, -17));
    let both = match deepest.sums {
        true => Element::new_item_with_sum_item(b"both".to_vec(), 5),
        false => Element::new_item(b"both".to_vec()),
    };
    recorder.insert(&deep, b"both", both);
    let mut target = deep.clone();
    target.push(b"item".to_vec());
    recorder.insert(
        &deep,
        b"reference",
        Element::new_reference(ReferencePathType::AbsolutePathReference(target)),
    );
    // A sixth tree, which nothing is written into.
    recorder.insert(&deep, b"unwritten", (deepest.make)());
    recorder.insert(&deep, b"item", Element::new_item(vec![1; 40]));
    recorder.insert(&deep, b"item", Element::new_item(vec![1; 40]));
    recorder.get(&deep, b"reference");
    recorder.delete(&deep, b"plus");

    // What a tree five deep refuses, it refuses after it was opened
    // through the trees above it: a sum item where it takes none, the
    // delete of a key it does not hold, and a write under such a key.
    if !deepest.sums {
        recorder.insert(&deep, b"no sum", Element::new_sum_item(1));
        refused += 1;
    }
    recorder.delete(&deep, b"nothing");
    let mut nowhere = deep.clone();
    nowhere.push(b"nothing".to_vec());
    recorder.insert(&nowhere, b"item", Element::new_item(vec![2; 5]));
    refused += 2;

    // Each tree above it is written into beside the tree it holds, and
    // its element is read: the root key and the aggregate it has by now.
    for level in (0..DEPTH).rev() {
        let at = &path[..level];
        let sums = level > 0 && KINDS[kinds[level - 1]].sums;
        recorder.insert(at, &beside, number(sums, level as i64 + 1));
        recorder.get(at, &path[level]);
    }
    for level in [3, 1] {
        recorder.delete(&path[..level], &beside);
    }

    // The deepest tree is emptied, which leaves its element with no root
    // key, and written into again.
    for key in [
        b"reference".as_slice(),
        b"both",
        b"item",
        b"long",
        b"minus",
        b"unwritten",
    ] {
        recorder.delete(&deep, key);
    }
    recorder.is_empty(&deep);
    recorder.get(&path[..DEPTH - 1], &path[DEPTH - 1]);
    recorder.insert(&deep, b"again", number(deepest.sums, 3));
    recorder.get(&[], top);
    refused
}

/// The kinds of the trees of a chain: the first, and each next one `stride`
/// kinds on, among the first `made` kinds.
fn kinds_of(first: usize, stride: usize, made: usize) -> [usize; DEPTH] {
    std::array::from_fn(|level| (first + level * stride) % made)
}

/// How many kinds of tree `version` makes: the kinds with a 128-bit sum
/// beside a count or in the hashes are made from `GROVE_V5` on.
fn made_by(version: &GroveVersion) -> usize {
    let insert = &version.grovedb_versions.operations.insert;
    match (
        insert.provable_count_provable_big_sum_tree_creation,
        insert.big_sum_tree_twins_creation,
    ) {
        (0, 0) => NARROW,
        _ => KINDS.len(),
    }
}

/// What a history on one engine gave.
struct History {
    answers: Vec<Answer>,
    /// The pairs of a tree and the tree in it that its chains held.
    pairs: BTreeSet<(usize, usize)>,
    /// How many calls were refused, and how many the history holds to be.
    refused: (usize, usize),
}

/// The history of a grove on `version`: a chain for every ordered pair of
/// kinds the version makes.
fn every_pair<E: Engine>(db: &GroveDb<E>, version: &'static GroveVersion) -> History {
    let made = made_by(version);
    let mut recorder = Recorder::new(db, version, None);
    let mut pairs = BTreeSet::new();
    let mut refused = 0;
    for stride in 0..made {
        for first in 0..made {
            let kinds = kinds_of(first, stride, made);
            let top = format!("chain {first:02} {stride:02}").into_bytes();
            refused += chain(&mut recorder, &top, kinds);
            pairs.extend(kinds.windows(2).map(|pair| (pair[0], pair[1])));
        }
    }
    History {
        refused: (recorder.refused, refused),
        answers: recorder.answers,
        pairs,
    }
}

/// The first calls of `answers` that were refused.
fn refusals(answers: &[Answer]) -> Vec<(&str, &str)> {
    let refused = answers.iter().filter(|answer| answer.2.starts_with("Err"));
    let refused = refused.map(|(call, _, answer)| (call.as_str(), answer.as_str()));
    refused.take(12).collect()
}

/// Opens the database in `dir` again, which nothing has open, and has
/// the physical checker read it, with every hash of every Merk node made
/// again by Merk's rules as `version` has them.
fn check(dir: &std::path::Path, version: &GroveVersion) {
    let db = Db::open(dir, &Options::default()).expect("the database opens again");
    let mut rules = MerkHashRules::for_check(version);
    let report = db.check(Scope::Still, Some(&mut rules));
    assert!(report.is_sound(), "{:#?}", report.findings);
    db.close().expect("a close");
}

fn assert_same(lignin: &[Answer], rocksdb: &[Answer]) {
    assert_eq!(lignin.len(), rocksdb.len());
    for (call, (lignin, rocksdb)) in lignin.iter().zip(rocksdb).enumerate() {
        assert_eq!(lignin, rocksdb, "call {call}");
    }
}

/// Every ordered pair of kinds as a tree and the tree in it, in chains
/// five trees deep, on one grove version.
fn every_pair_of_kinds_on(version: &'static GroveVersion) {
    let made = made_by(version);
    let dir = tempfile::tempdir().expect("a directory");
    let history = {
        let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
        let history = every_pair(&db, version);
        db.flush().expect("a flush");
        history
    };

    #[cfg(feature = "rocksdb_storage")]
    {
        let dir = tempfile::tempdir().expect("a directory");
        let db = GroveDb::<crate::RocksDbEngine>::open_with_engine(dir.path()).expect("RocksDB");
        let on_rocksdb = every_pair(&db, version);
        assert_same(&history.answers, &on_rocksdb.answers);
    }

    // What the history held: every pair of kinds, and no refusal but those
    // it made on purpose.
    assert_eq!(history.pairs.len(), made * made);
    let (refused, on_purpose) = history.refused;
    assert_eq!(refused, on_purpose, "{:#?}", refusals(&history.answers));
    check(dir.path(), version);
    eprintln!(
        "{} chains of {made} kinds: {} calls, {refused} of them refused",
        made * made,
        history.answers.len()
    );
}

#[test]
fn nested_inserts_five_levels_deep_on_grove_v1() {
    every_pair_of_kinds_on(&GROVE_VERSIONS[0]);
}

#[test]
fn nested_inserts_five_levels_deep_on_grove_v2() {
    every_pair_of_kinds_on(&GROVE_VERSIONS[1]);
}

#[test]
fn nested_inserts_five_levels_deep_on_grove_v3() {
    every_pair_of_kinds_on(&GROVE_VERSIONS[2]);
}

#[test]
fn nested_inserts_five_levels_deep_on_grove_v4() {
    every_pair_of_kinds_on(&GROVE_VERSIONS[3]);
}

#[test]
fn nested_inserts_five_levels_deep_on_grove_v5() {
    every_pair_of_kinds_on(&GROVE_VERSIONS[4]);
}

#[test]
fn the_grove_versions_make_nine_kinds_of_tree_and_the_latest_twelve() {
    let made: Vec<usize> = GROVE_VERSIONS.iter().map(made_by).collect();
    assert_eq!(made, [NARROW, NARROW, NARROW, NARROW, KINDS.len()]);
    let names: BTreeSet<&str> = KINDS.iter().map(|kind| kind.name).collect();
    assert_eq!(names.len(), KINDS.len());
    // Each is a tree of a type of its own that keeps its data in one Merk,
    // and takes sum items where the history writes them.
    let mut types = Vec::new();
    for kind in &KINDS {
        let element = (kind.make)();
        let tree_type = element.tree_type().expect("a tree");
        assert!(!element.uses_non_merk_data_storage() && !tree_type.is_indexed_primary());
        assert_eq!(tree_type.allows_sum_item(), kind.sums, "{}", kind.name);
        types.push(tree_type.discriminant());
    }
    types.sort_unstable();
    types.dedup();
    assert_eq!(types.len(), KINDS.len());
    // A kind a version does not make is refused by it, on both engines with
    // the same words, and leaves the root hash as it was.
    for version in &GROVE_VERSIONS[..4] {
        let dir = tempfile::tempdir().expect("a directory");
        let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
        let mut recorder = Recorder::new(&db, version, None);
        for kind in &KINDS[NARROW..] {
            recorder.insert(&[], kind.name.as_bytes(), (kind.make)());
        }
        assert_eq!(recorder.refused, KINDS.len() - NARROW);
        #[cfg(feature = "rocksdb_storage")]
        {
            let dir = tempfile::tempdir().expect("a directory");
            let db =
                GroveDb::<crate::RocksDbEngine>::open_with_engine(dir.path()).expect("RocksDB");
            let mut on_rocksdb = Recorder::new(&db, version, None);
            for kind in &KINDS[NARROW..] {
                on_rocksdb.insert(&[], kind.name.as_bytes(), (kind.make)());
            }
            assert_same(&recorder.answers, &on_rocksdb.answers);
        }
    }
}
