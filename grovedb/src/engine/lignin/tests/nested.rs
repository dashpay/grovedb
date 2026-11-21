//! Root-hash propagation through five levels of trees (GROVE-02): the same
//! calls on LigninDB and on RocksDB, with every answer, every
//! `OperationCost` and the root hash after every write compared.
//!
//! A chain is five trees one inside the other under the root. The kinds of
//! tree are the twelve whose data is one Merk. An indexed tree, whose
//! element hosts more than one Merk, is in no chain: the engine refuses a
//! write into one (GROVE-04). On each grove version the chains cover every
//! ordered pair of the kinds that version makes, as a tree and the tree in
//! it.
//!
//! - `every_pair`: chains written with no transaction.
//! - `in_transactions`: chains made inside write transactions, with a
//!   savepoint that is rolled back to and a transaction that is rolled
//!   back whole, and a second session on the database opened again.
//! - `backward_references`: elements that take backward references and
//!   references to them, five trees deep, which GroveDB writes and
//!   propagates through its Merk cache (`crate::merk_cache`).
//! - `refused_fetches`: handles that refuse one fetch during an insert,
//!   at each level of a chain: the operation ends with the error, the
//!   node records of the chain's Merks are what they were, and the next
//!   operation reads the stored state.
//!
//! Each history runs in a database of its own. The LigninDB database is
//! then closed, opened again and read by the physical checker of the
//! engine, which makes the hashes of the Merk nodes again by Merk's rules
//! and compares them with the stored ones. A value hash that only its
//! writer can make, which is a reference's, is taken as stored.

use std::{collections::BTreeSet, path::Path};

use grovedb_costs::{CostResult, OperationCost};
use grovedb_merk::{
    element::tree_type::ElementTreeTypeExtensions,
    store::{lignin::hash::MerkHashRules, NodeStore},
};
use grovedb_storage::RawIterator;
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};
use lignindb::{verify::Scope, Db, Options};

use super::LigninEngine;
use crate::{
    engine::Engine,
    reference_path::ReferencePathType,
    tests::faultable_reads::{with_one_failed_fetch, RefusedReads, INJECTED_READ_FAILURE},
    Element, Error, GroveDb, TransactionArg, TreeType,
};

/// How many trees a chain has, one inside the other.
const DEPTH: usize = 5;

/// A kind of tree whose data is one Merk.
struct Kind {
    name: &'static str,
    make: fn() -> Element,
    /// Whether the tree takes sum items.
    sums: bool,
}

/// The kinds of tree every grove version makes.
const NARROW: usize = 9;

const fn kind(name: &'static str, make: fn() -> Element, sums: bool) -> Kind {
    Kind { name, make, sums }
}

/// Every kind, the kinds that only `GROVE_V5` makes last.
const KINDS: [Kind; 12] = [
    kind("tree", Element::empty_tree, false),
    kind("sum tree", Element::empty_sum_tree, true),
    kind("big sum tree", Element::empty_big_sum_tree, true),
    kind("count tree", Element::empty_count_tree, false),
    kind("count sum tree", Element::empty_count_sum_tree, true),
    kind(
        "provable count tree",
        Element::empty_provable_count_tree,
        false,
    ),
    kind(
        "provable count sum tree",
        Element::empty_provable_count_sum_tree,
        true,
    ),
    kind("provable sum tree", Element::empty_provable_sum_tree, true),
    kind(
        "provable count provable sum tree",
        Element::empty_provable_count_provable_sum_tree,
        true,
    ),
    kind(
        "provable count provable big sum tree",
        Element::empty_provable_count_provable_big_sum_tree,
        true,
    ),
    kind(
        "count big sum tree",
        Element::empty_count_big_sum_tree,
        true,
    ),
    kind(
        "provable big sum tree",
        Element::empty_provable_big_sum_tree,
        true,
    ),
];

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

/// The kinds of the trees of a chain: the first, and each next one `stride`
/// kinds on, among the first `made` kinds.
fn kinds_of(first: usize, stride: usize, made: usize) -> [usize; DEPTH] {
    std::array::from_fn(|level| (first + level * stride) % made)
}

/// One call of a history: what it was, what it was charged, and what it
/// returned or how it failed, as text.
type Answer = (String, OperationCost, String);

/// What a history gave on one engine.
#[derive(Default)]
struct Log {
    answers: Vec<Answer>,
    /// How many calls were refused, and how many the history made to be.
    refused: usize,
    on_purpose: usize,
    /// The pairs of a tree and the tree in it that its chains held.
    pairs: BTreeSet<(usize, usize)>,
}

impl Log {
    fn answer<T: std::fmt::Debug>(&mut self, name: String, answer: CostResult<T, Error>) {
        let cost = answer.cost.clone();
        let value = answer.unwrap();
        self.refused += usize::from(value.is_err());
        self.answers.push((name, cost, format!("{value:?}")));
    }

    /// The answer of the last call, as text.
    fn last(&self) -> &str {
        &self.answers.last().expect("a call").2
    }

    /// The history made no refusal but those it made on purpose.
    fn assert_refusals(&self) {
        let refused: Vec<(&str, &str)> = (self.answers.iter())
            .filter(|(_, _, answer)| answer.starts_with("Err"))
            .map(|(call, _, answer)| (call.as_str(), answer.as_str()))
            .take(12)
            .collect();
        assert_eq!(self.refused, self.on_purpose, "{refused:#?}");
    }
}

/// The calls of a history through one transaction, or through none.
struct Recorder<'db, 'a, 'l, E: Engine> {
    db: &'db GroveDb<E>,
    version: &'static GroveVersion,
    through: TransactionArg<'db, 'a, E>,
    log: &'l mut Log,
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

impl<'db, 'a, 'l, E: Engine> Recorder<'db, 'a, 'l, E> {
    fn new(
        db: &'db GroveDb<E>,
        version: &'static GroveVersion,
        through: TransactionArg<'db, 'a, E>,
        log: &'l mut Log,
    ) -> Self {
        Self {
            db,
            version,
            through,
            log,
        }
    }

    /// The root hash as the history's transaction reads it.
    fn root(&mut self) {
        let root = self.db.root_hash(self.through, self.version);
        self.log.answer("root".to_owned(), root);
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
        self.log.answer(named("insert", path, key), inserted);
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
        self.log.answer(named("delete", path, key), deleted);
        self.root();
    }

    fn get(&mut self, path: &[Vec<u8>], key: &[u8]) {
        let read = self
            .db
            .get(segments(path).as_slice(), key, self.through, self.version);
        self.log.answer(named("get", path, key), read);
    }

    /// Reads an element as [`Self::get`] does, and gives the type of tree
    /// that it is.
    fn tree_at(&mut self, path: &[Vec<u8>], key: &[u8]) -> Option<TreeType> {
        let read = self
            .db
            .get(segments(path).as_slice(), key, self.through, self.version);
        let tree_type = read.value.as_ref().ok().and_then(Element::tree_type);
        self.log.answer(named("get", path, key), read);
        tree_type
    }

    fn is_empty(&mut self, path: &[Vec<u8>]) {
        let empty = self
            .db
            .is_empty_tree(segments(path).as_slice(), self.through, self.version);
        self.log.answer(named("empty", path, b""), empty);
    }

    /// A call that the history makes to be refused.
    fn refused(&mut self, call: impl FnOnce(&mut Self)) {
        let before = self.log.refused;
        call(self);
        assert_eq!(self.log.refused, before + 1, "{}", self.log.last());
        self.log.on_purpose += 1;
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

/// The path of the deepest tree of the chain under the key `top` of the
/// root.
fn deepest(top: &[u8]) -> Vec<Vec<u8>> {
    let below = (2..=DEPTH).map(|level| format!("level {level}").into_bytes());
    std::iter::once(top.to_vec()).chain(below).collect()
}

/// Makes the five trees of the chain under `top`, of the `kinds` given, one
/// inside the other. Returns the path of the deepest.
fn trees<E: Engine>(
    recorder: &mut Recorder<'_, '_, '_, E>,
    top: &[u8],
    kinds: [usize; DEPTH],
) -> Vec<Vec<u8>> {
    let path = deepest(top);
    for (level, kind) in kinds.iter().enumerate() {
        let tree = (KINDS[*kind].make)();
        let (at, key) = (&path[..level], &path[level]);
        recorder.insert(at, key, tree.clone());
        // The tree that is there is of the kind its pair is counted for.
        assert_eq!(recorder.tree_at(at, key), tree.tree_type());
    }
    recorder
        .log
        .pairs
        .extend(kinds.windows(2).map(|pair| (pair[0], pair[1])));
    path
}

/// The node records of the Merks of a chain, the root Merk first and the
/// Merk of the deepest tree last: every key with the record that a cursor
/// of a read transaction shows for it, which holds the node's value, its
/// feature, its hashes and its links, in hexadecimal.
fn stored<E: Engine>(db: &GroveDb<E>, chain: &[Vec<u8>]) -> String {
    let engine = db.engine();
    let transaction = engine.start_read_transaction();
    let mut records = String::new();
    for depth in 0..=chain.len() {
        let path = segments(&chain[..depth]);
        let handle = engine.subtree(path.as_slice().into(), None, &transaction);
        let handle = handle.unwrap();
        let mut cursor = NodeStore::cursor(&handle);
        cursor.seek_to_first().unwrap();
        while cursor.valid().unwrap() {
            let key = hex::encode(cursor.key().unwrap().expect("a key"));
            let record = hex::encode(cursor.value().unwrap().expect("a record"));
            records.push_str(&format!("{depth} {key} {record}\n"));
            cursor.next().unwrap();
        }
        cursor.status().expect("a cursor that walked");
    }
    records
}

/// The records of [`stored`] that are of the Merk `depth` trees deep.
fn of_depth(records: &str, depth: usize) -> Vec<&str> {
    let of_depth = format!("{depth} ");
    let records = records.lines();
    records
        .filter(|record| record.starts_with(&of_depth))
        .collect()
}

/// A chain under the key `top` of the root: five trees of the `kinds`
/// given, one inside the other, and then writes at every level of it, the
/// deepest first.
fn chain<E: Engine>(recorder: &mut Recorder<'_, '_, '_, E>, top: &[u8], kinds: [usize; DEPTH]) {
    let path = trees(recorder, top, kinds);
    let deepest = &KINDS[kinds[DEPTH - 1]];
    let beside = [top, b" beside"].concat();

    // The deepest tree is written into. A write that changes it changes
    // the root hash of the five trees and of the root.
    recorder.insert(&path, b"item", Element::new_item(vec![kinds[0] as u8; 9]));
    recorder.insert(&path, b"long", Element::new_item(vec![7; 300]));
    recorder.insert(&path, b"plus", number(deepest.sums, 41));
    recorder.insert(&path, b"minus", number(deepest.sums, -17));
    let both = match deepest.sums {
        true => Element::new_item_with_sum_item(b"both".to_vec(), 5),
        false => Element::new_item(b"both".to_vec()),
    };
    recorder.insert(&path, b"both", both);
    let mut target = path.clone();
    target.push(b"item".to_vec());
    recorder.insert(
        &path,
        b"reference",
        Element::new_reference(ReferencePathType::AbsolutePathReference(target)),
    );
    // A sixth tree, which nothing is written into.
    recorder.insert(&path, b"unwritten", (deepest.make)());
    recorder.insert(&path, b"item", Element::new_item(vec![1; 40]));
    recorder.insert(&path, b"item", Element::new_item(vec![1; 40]));
    recorder.get(&path, b"reference");
    recorder.delete(&path, b"plus");

    // What a tree five deep refuses, it refuses after it was opened
    // through the trees above it: a sum item where it takes none, the
    // delete of a key it does not hold, and a write under such a key.
    if !deepest.sums {
        recorder.refused(|recorder| recorder.insert(&path, b"no sum", Element::new_sum_item(1)));
    }
    recorder.refused(|recorder| recorder.delete(&path, b"nothing"));
    let mut nowhere = path.clone();
    nowhere.push(b"nothing".to_vec());
    recorder.refused(|recorder| recorder.insert(&nowhere, b"item", Element::new_item(vec![2; 5])));

    // Each Merk above it is written into beside the tree it holds, and
    // the element of that tree is read: the root key and the aggregate it
    // has by now.
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
        recorder.delete(&path, key);
    }
    recorder.is_empty(&path);
    recorder.get(&path[..DEPTH - 1], &path[DEPTH - 1]);
    recorder.insert(&path, b"again", number(deepest.sums, 3));
    recorder.get(&[], top);
}

fn open<E: Engine>(dir: &Path) -> GroveDb<E> {
    GroveDb::<E>::open_with_engine(dir).expect("a database")
}

/// Opens the database in `dir` again, which nothing has open, and has
/// the physical checker read it: the hashes of its Merk nodes are made
/// again by Merk's rules as `version` has them and compared with the
/// stored ones, a value hash that only its writer can make being taken as
/// stored.
fn check(dir: &Path, version: &GroveVersion) {
    let db = Db::open(dir, &Options::default()).expect("the database opens again");
    let mut rules = MerkHashRules::for_check(version);
    let report = db.check(Scope::Still, Some(&mut rules));
    assert!(report.is_sound(), "{:#?}", report.findings);
    db.close().expect("a close");
}

fn assert_same(lignin: &Log, rocksdb: &Log) {
    assert_eq!(lignin.answers.len(), rocksdb.answers.len());
    for (call, (lignin, rocksdb)) in lignin.answers.iter().zip(&rocksdb.answers).enumerate() {
        assert_eq!(lignin, rocksdb, "call {call}");
    }
}

/// Runs a history, which is a function of an engine, a directory and a
/// grove version, on LigninDB, and where the RocksDB engine is compiled in
/// on RocksDB too, and compares every answer. Then the history's own
/// refusals are counted, and the physical checker reads the LigninDB
/// database it left. Gives the history's log on LigninDB.
macro_rules! on_both_engines {
    ($history:ident, $version:expr) => {{
        let version: &'static GroveVersion = $version;
        let dir = tempfile::tempdir().expect("a directory");
        let log = $history::<LigninEngine>(dir.path(), version);
        #[cfg(feature = "rocksdb_storage")]
        {
            let dir = tempfile::tempdir().expect("a directory");
            let on_rocksdb = $history::<crate::RocksDbEngine>(dir.path(), version);
            assert_same(&log, &on_rocksdb);
        }
        log.assert_refusals();
        check(dir.path(), version);
        eprintln!(
            "{} on grove version {}: {} calls, {} of them refused",
            stringify!($history),
            version.protocol_version,
            log.answers.len(),
            log.refused
        );
        log
    }};
}

/// A chain for every ordered pair of the kinds `version` makes, written
/// with no transaction.
fn every_pair<E: Engine>(dir: &Path, version: &'static GroveVersion) -> Log {
    let db = open::<E>(dir);
    let made = made_by(version);
    let mut log = Log::default();
    let mut recorder = Recorder::new(&db, version, None, &mut log);
    for stride in 0..made {
        for first in 0..made {
            let top = format!("chain {first:02} {stride:02}").into_bytes();
            chain(&mut recorder, &top, kinds_of(first, stride, made));
        }
    }
    db.flush().expect("a flush");
    log
}

fn every_pair_of_kinds_on(version: &'static GroveVersion) {
    let made = made_by(version);
    let log = on_both_engines!(every_pair, version);
    assert_eq!(log.pairs.len(), made * made);
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

/// Chains made inside write transactions, and a second session.
///
/// A transaction makes a chain, sets a savepoint, makes another and
/// writes into trees from before the savepoint, rolls back to it, makes
/// the undone chain again of other kinds, and commits. A second
/// transaction makes a chain, is rolled back whole, makes another and
/// commits. The database is then closed and opened again, and written at
/// the bottom of every chain it holds.
fn in_transactions<E: Engine>(dir: &Path, version: &'static GroveVersion) -> Log {
    let made = made_by(version);
    let mut log = Log::default();
    let item = |n: u8| Element::new_item(vec![n; 12]);
    let first_root = {
        let db = open::<E>(dir);
        chain(
            &mut Recorder::new(&db, version, None, &mut log),
            b"committed",
            kinds_of(0, 1, made),
        );

        let tx = db.start_transaction();
        let mut inside = Recorder::new(&db, version, Some(&tx), &mut log);
        chain(&mut inside, b"kept", kinds_of(1, 2, made));
        inside.insert(&deepest(b"committed"), b"in the transaction", item(3));
        db.set_savepoint(&tx).expect("a savepoint");
        chain(&mut inside, b"undone", kinds_of(2, 3, made));
        inside.insert(&deepest(b"kept"), b"undone", item(4));
        inside.insert(&deepest(b"committed"), b"undone", item(5));
        db.rollback_to_savepoint(&tx).expect("a rollback");
        inside.root();
        inside.refused(|inside| inside.get(&[], b"undone"));
        inside.refused(|inside| inside.get(&deepest(b"kept"), b"undone"));
        inside.get(&deepest(b"committed"), b"in the transaction");
        // The trees that were rolled back are made again, of other kinds.
        chain(&mut inside, b"undone", kinds_of(3, 4, made));
        // Nothing of the transaction is committed yet.
        let mut outside = Recorder::new(&db, version, None, &mut log);
        outside.root();
        outside.refused(|outside| outside.get(&[], b"kept"));
        log.answer("commit".to_owned(), db.commit_transaction(tx));
        let mut outside = Recorder::new(&db, version, None, &mut log);
        outside.root();
        outside.get(&deepest(b"kept"), b"again");
        outside.get(&deepest(b"undone"), b"again");

        let tx = db.start_transaction();
        chain(
            &mut Recorder::new(&db, version, Some(&tx), &mut log),
            b"given up",
            kinds_of(4, 5, made),
        );
        db.rollback_transaction(&tx).expect("a rollback");
        let mut inside = Recorder::new(&db, version, Some(&tx), &mut log);
        inside.root();
        inside.refused(|inside| inside.get(&[], b"given up"));
        chain(&mut inside, b"after it", kinds_of(5, 6, made));
        inside.insert(&deepest(b"undone"), b"later", item(6));
        log.answer("commit".to_owned(), db.commit_transaction(tx));
        Recorder::new(&db, version, None, &mut log).root();
        db.flush().expect("a flush");
        log.last().to_owned()
    };

    // The next session reads what the first one committed, and writes on.
    let db = open::<E>(dir);
    let mut session = Recorder::new(&db, version, None, &mut log);
    session.root();
    assert_eq!(session.log.last(), first_root);
    session.refused(|session| session.get(&[], b"given up"));
    for top in [b"committed".as_slice(), b"kept", b"undone", b"after it"] {
        session.get(&deepest(top), b"again");
        session.insert(&deepest(top), b"second session", item(7));
        session.get(&[], top);
    }
    chain(&mut session, b"second session", kinds_of(6, 7, made));
    db.flush().expect("a flush");
    log
}

#[test]
fn chains_in_transactions_and_in_a_second_session_on_every_grove_version() {
    for version in GROVE_VERSIONS {
        on_both_engines!(in_transactions, version);
    }
}

fn bidirectional(to: &[Vec<u8>], key: &[u8]) -> Element {
    let mut target = to.to_vec();
    target.push(key.to_vec());
    Element::new_bidirectional_reference_with_options(
        ReferencePathType::AbsolutePathReference(target),
        None,
        true,
        None,
    )
}

/// Elements that take backward references, and the references to them,
/// five trees deep. There is a round for each kind of tree the version
/// makes, which is the kind at the bottom of the round's first chain: an
/// item at the bottom of that chain, a reference to it at the bottom of a
/// second chain and a reference to that one at the bottom of a third.
/// GroveDB makes each of these writes through a Merk cache, which also
/// carries the root hashes up: the item's tree and the reference's tree
/// are both five deep, and their chains meet at the root.
fn backward_references<E: Engine>(dir: &Path, version: &'static GroveVersion) -> Log {
    let db = open::<E>(dir);
    let made = made_by(version);
    let mut log = Log::default();
    let mut recorder = Recorder::new(&db, version, None, &mut log);
    for round in 0..made {
        let name = |chain: &str| format!("{chain} {round:02}").into_bytes();
        let kinds = kinds_of(round, 1, made);
        let item = trees(&mut recorder, &name("item"), kinds);
        let first = trees(&mut recorder, &name("first"), kinds_of(round, 2, made));
        let second = trees(&mut recorder, &name("second"), kinds_of(round, 3, made));
        let sums = KINDS[kinds[DEPTH - 1]].sums;
        let referred =
            |value: &[u8]| Element::new_item_allowing_bidirectional_references(value.to_vec());

        recorder.insert(&item, b"plain", Element::new_item(vec![9; 20]));
        recorder.insert(&item, b"referred", referred(b"a value"));
        recorder.insert(&first, b"reference", bidirectional(&item, b"referred"));
        recorder.insert(&second, b"reference", bidirectional(&first, b"reference"));
        recorder.get(&item, b"referred");
        recorder.get(&first, b"reference");
        recorder.get(&second, b"reference");
        // The item is written again: the hash of its value reaches both
        // references, each five trees deep in a chain of its own.
        recorder.insert(&item, b"referred", referred(b"another value"));
        recorder.insert(&item, b"referred", referred(b"another value"));
        recorder.get(&second, b"reference");
        if sums {
            let sum = Element::new_sum_item_allowing_bidirectional_references(7);
            recorder.insert(&item, b"sum", sum);
            recorder.insert(&first, b"to the sum", bidirectional(&item, b"sum"));
            recorder.get(&first, b"to the sum");
            recorder.delete(&first, b"to the sum");
            recorder.get(&item, b"sum");
        }
        // The last reference of the chain is deleted, and the one it
        // pointed at no longer lists it.
        recorder.delete(&second, b"reference");
        recorder.get(&first, b"reference");
        // The item is deleted, and its referrer with it.
        recorder.delete(&item, b"referred");
        recorder.refused(|recorder| recorder.get(&first, b"reference"));
        recorder.get(&item, b"plain");
        recorder.get(&[], &name("first"));
    }
    db.flush().expect("a flush");
    log
}

#[test]
fn backward_references_five_levels_deep_on_grove_v4() {
    on_both_engines!(backward_references, &GROVE_VERSIONS[3]);
}

#[test]
fn backward_references_five_levels_deep_on_grove_v5() {
    on_both_engines!(backward_references, &GROVE_VERSIONS[4]);
}

/// The keys of the nodes whose fetch [`refused_fetches_of`] refuses, one
/// at a time: a node at the bottom of the chain, and the element of each
/// tree of the chain, the deepest first.
fn fetched() -> Vec<Vec<u8>> {
    let mut keys = vec![b"c".to_vec()];
    keys.extend(deepest(b"chain").into_iter().rev());
    keys
}

/// A store that refuses one read inside a write operation.
///
/// The chain is five trees deep, and each of the five Merks that hold a
/// tree of it, the root Merk among them, holds a key before that tree, so
/// that the element of the tree is not the root of its Merk: a write of it
/// has to fetch it. The Merk at the bottom holds
/// `b` with `a` and `c` under it. Then, for each key of [`fetched`], an
/// insert of a new key at the bottom is made with the first fetch of that
/// key refused. For `c` that is a fetch in the walk of the insert's own
/// write on grove versions 1 to 3, and on 4 and 5 in the walk that reads
/// the old value before the write. For the element of a tree it is a
/// fetch in the write that carries the new root hash into the tree above.
/// The insert must fail with the refusal and leave the root hash and
/// every node record of the six Merks as they were before it, and the
/// same insert made again must succeed. After each of these inserts that
/// succeeds, the records are logged, so that both engines are held to the
/// same ones. A refused insert logs no records: they are compared with
/// those from before it.
///
/// An insert that is made changes a record of each of the six Merks,
/// which the history asserts. In the root Merk and the three below it the
/// tree on the way keeps its root key, so what changes there is hashes
/// alone: the records show a write that stores nothing but a hash.
///
/// With `referred`, the inserted element is one that takes backward
/// references, which GroveDB inserts through its Merk cache: there the
/// refused fetches of the tree elements are fetches inside a write closure
/// of the cache. With `refuse` unset nothing is refused, and the refused
/// inserts are left out with the reads around them.
fn refused_fetches_of<E: Engine>(
    dir: &Path,
    version: &'static GroveVersion,
    referred: bool,
    refuse: bool,
) -> Log {
    let db = open::<RefusedReads<E>>(dir);
    let mut log = Log::default();
    let mut recorder = Recorder::new(&db, version, None, &mut log);
    let deep = deepest(b"chain");
    for level in 0..DEPTH {
        recorder.insert(
            &deep[..level],
            b"a",
            Element::new_item(vec![level as u8; 4]),
        );
        recorder.insert(&deep[..level], &deep[level], Element::empty_tree());
    }
    for key in [b"b", b"a", b"c"] {
        recorder.insert(&deep, key, Element::new_item(key.to_vec()));
    }
    let element = |n: usize| match referred {
        true => Element::new_item_allowing_bidirectional_references(vec![n as u8; 6]),
        false => Element::new_item(vec![n as u8; 6]),
    };
    for (n, refused) in fetched().iter().enumerate() {
        let key = format!("d{n}").into_bytes();
        let before = stored(recorder.db, &deep);
        if refuse {
            recorder.root();
            let root = recorder.log.last().to_owned();
            let (_, fired) = with_one_failed_fetch(refused, || {
                recorder.refused(|recorder| recorder.insert(&deep, &key, element(n)));
            });
            assert!(fired, "{}", String::from_utf8_lossy(refused));
            // The insert failed with the refusal of the store, and nothing
            // of it is stored: the root hash that was read after it is the
            // one from before, every node record of the six Merks is the
            // record from before, and the new key is not there.
            let [.., (_, _, failed), (_, _, after)] = recorder.log.answers.as_slice() else {
                panic!("an insert is logged with the root hash it left")
            };
            assert!(failed.contains(INJECTED_READ_FAILURE), "{failed}");
            assert_eq!(after, &root);
            assert_eq!(stored(recorder.db, &deep), before);
            recorder.refused(|recorder| recorder.get(&deep, &key));
            let missing = recorder.log.last();
            assert!(missing.starts_with("Err(PathKeyNotFound("), "{missing}");
        }
        recorder.insert(&deep, &key, element(n));
        recorder.get(&deep, &key);
        let after = stored(recorder.db, &deep);
        for depth in 0..=DEPTH {
            let (before, after) = (of_depth(&before, depth), of_depth(&after, depth));
            assert_ne!(before, after, "the Merk {depth} trees deep");
        }
        let logged = ("stored".to_owned(), OperationCost::default(), after);
        recorder.log.answers.push(logged);
    }
    db.flush().expect("a flush");
    log
}

fn refused_fetches<E: Engine>(dir: &Path, version: &'static GroveVersion) -> Log {
    refused_fetches_of::<E>(dir, version, false, true)
}

fn refused_fetches_through_the_merk_cache<E: Engine>(
    dir: &Path,
    version: &'static GroveVersion,
) -> Log {
    refused_fetches_of::<E>(dir, version, true, true)
}

/// The inserts of a history that were made, each with its cost and the
/// root hash it left.
fn made(log: &Log) -> Vec<&[Answer]> {
    let made = |pair: &&[Answer]| pair[0].0.starts_with("insert") && pair[0].2.starts_with("Ok");
    log.answers.windows(2).filter(made).collect()
}

/// A history of refused fetches left what the same history leaves with
/// nothing refused: every insert that was made cost the same and left the
/// same root hash.
fn assert_as_never_refused(log: &Log, version: &'static GroveVersion, referred: bool) {
    let dir = tempfile::tempdir().expect("a directory");
    let clean = refused_fetches_of::<LigninEngine>(dir.path(), version, referred, false);
    check(dir.path(), version);
    assert_eq!(clean.refused, 0);
    assert_eq!(made(log), made(&clean));
    assert_eq!(made(log).len(), 2 * DEPTH + 3 + fetched().len());
}

#[test]
fn a_read_refused_inside_a_write_ends_the_operation_and_leaves_the_stored_state() {
    for version in GROVE_VERSIONS {
        let log = on_both_engines!(refused_fetches, version);
        assert_as_never_refused(&log, version, false);
    }
}

#[test]
fn a_read_refused_inside_a_write_through_the_merk_cache_leaves_the_stored_state() {
    for version in &GROVE_VERSIONS[3..] {
        let log = on_both_engines!(refused_fetches_through_the_merk_cache, version);
        assert_as_never_refused(&log, version, true);
    }
}

#[test]
fn the_merk_cache_drops_a_merk_whose_write_failed_on_lignindb() {
    crate::merk_cache::failed_writes::a_merk_whose_write_failed_is_dropped::<LigninEngine>();
}

#[test]
fn the_grove_versions_make_nine_kinds_of_tree_and_the_latest_twelve() {
    let made: Vec<usize> = GROVE_VERSIONS.iter().map(made_by).collect();
    assert_eq!(made, [NARROW, NARROW, NARROW, NARROW, KINDS.len()]);
    let names: BTreeSet<&str> = KINDS.iter().map(|kind| kind.name).collect();
    assert_eq!(names.len(), KINDS.len());
    // Each is a tree of a type of its own that keeps its data in one Merk,
    // and takes sum items where the histories write them.
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
}

/// The kinds a grove version does not make: each is refused, with the root
/// hash left as it was.
fn kinds_not_made<E: Engine>(dir: &Path, version: &'static GroveVersion) -> Log {
    let db = open::<E>(dir);
    let mut log = Log::default();
    let mut recorder = Recorder::new(&db, version, None, &mut log);
    recorder.root();
    let root = recorder.log.last().to_owned();
    for kind in &KINDS[made_by(version)..] {
        recorder.refused(|recorder| recorder.insert(&[], kind.name.as_bytes(), (kind.make)()));
        assert_eq!(recorder.log.last(), root);
    }
    log
}

#[test]
fn a_kind_of_tree_that_a_grove_version_does_not_make_is_refused_by_both_engines() {
    for version in GROVE_VERSIONS {
        let log = on_both_engines!(kinds_not_made, version);
        assert_eq!(log.refused, KINDS.len() - made_by(version));
    }
}
