//! A step of a cursor from no key (D-189): what RocksDB does, and that
//! the cursors of the store do the same.
//!
//! RocksDB keeps the records of every subtree of one keyspace in one
//! column family, under `prefix ‖ key`, and a cursor of a subtree is an
//! iterator of the column family that shows a key of another subtree as no
//! key. So the iterator stands somewhere on the line of all stored keys of
//! the column, or nowhere, and a step moves it along that line: from past
//! the end of its subtree back onto the last key, if a record of another
//! subtree follows in the column, and nowhere if none does.
//!
//! [`Line`] is that as a model. The first test holds RocksDB to it, for
//! every cursor of the seam. The others hold LigninDB to RocksDB: the same
//! places, the same records and the same cost after every move.

use std::collections::{BTreeMap, BTreeSet};

use grovedb_query::{Query, QueryItem};
use grovedb_storage::{rocksdb_storage::RocksDbStorage, FlatStore};
use lignindb::{
    commit::{Committed, NoStaleHashes},
    Db as Database, Options,
};

use super::{
    super::{LigninSnapshot, MODEL},
    *,
};
use crate::merk::KVIterator;

type RocksTransaction<'a> = <RocksDbStorage as Storage<'a>>::Transaction;

/// What a cursor of the seam walks: the nodes of a Merk, the flat records
/// of a subtree's data, its aux records or its meta records.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Space {
    Nodes,
    Flat,
    Aux,
    Meta,
}

const SPACES: [Space; 4] = [Space::Nodes, Space::Flat, Space::Aux, Space::Meta];

impl Space {
    /// The column family RocksDB keeps the records in: the nodes of every
    /// Merk and the flat data of every tree that keeps it flat are in the
    /// default one.
    fn column(self) -> u8 {
        match self {
            Self::Nodes | Self::Flat => 0,
            Self::Aux => 1,
            Self::Meta => 2,
        }
    }
}

/// Calls `$call` with a cursor of `$space` of `$handle`, a handle of
/// either engine.
macro_rules! with_cursor {
    ($space:expr, $handle:expr, $call:expr) => {
        match $space {
            Space::Nodes => ($call)(NodeStore::cursor(&$handle)),
            Space::Flat => ($call)(FlatStore::cursor(&$handle)),
            Space::Aux => {
                let keyspace = $handle.aux();
                let shown = ($call)(keyspace.cursor());
                shown
            }
            Space::Meta => {
                let keyspace = $handle.meta();
                let shown = ($call)(keyspace.cursor());
                shown
            }
        }
    };
}

/// Puts the records `puts` into a flat keyspace and deletes `deletes`.
fn write_flat<S: FlatStore>(keyspace: &S, puts: &[u64], deletes: &[u64]) {
    for key in puts {
        let key = be(*key);
        (keyspace.put(&key, &flat_value(&key), None).unwrap()).expect("a put");
    }
    for key in deletes {
        (keyspace.delete(be(*key), None).unwrap()).expect("a delete");
    }
}

/// Writes flat records of `$space` through `$handle`, a handle of either
/// engine.
macro_rules! write_records {
    ($space:expr, $handle:expr, $puts:expr, $deletes:expr) => {
        match $space {
            Space::Flat => write_flat(&$handle, $puts, $deletes),
            Space::Aux => write_flat(&$handle.aux(), $puts, $deletes),
            Space::Meta => write_flat(&$handle.meta(), $puts, $deletes),
            Space::Nodes => unreachable!("nodes are written by a Merk"),
        }
    };
}

fn flat_value(key: &[u8]) -> Vec<u8> {
    [b"the record under ", key].concat()
}

/// One write to the grove of a test, the same on both engines.
#[derive(Clone, Debug)]
enum Step {
    /// A batch of a subtree's Merk: puts of the first keys, deletes of the
    /// others.
    Merk(Prefix, Vec<u64>, Vec<u64>),
    /// Puts and deletes of flat records of a subtree.
    Records(Prefix, Space, Vec<u64>, Vec<u64>),
    /// The subtree is entered and holds nothing. RocksDB has no word for
    /// that.
    Enter(Prefix),
    /// The nodes of the subtree's Merk are removed (`Merk::clear`).
    Clear(Prefix),
    /// Every record of the subtree, in every keyspace, is removed.
    Drop(Prefix),
    /// The transaction is committed and another begun.
    Commit,
}

/// Every record of a grove as RocksDB keeps it: the stored keys of each
/// column family.
#[derive(Clone, Debug, Default)]
struct Model {
    stored: BTreeSet<(u8, Vec<u8>)>,
    /// The subtrees that hold a record or were entered, with whether the
    /// data of each is flat.
    subtrees: BTreeMap<Prefix, bool>,
}

fn stored(prefix: &Prefix, key: &[u8]) -> Vec<u8> {
    [&prefix[..], key].concat()
}

impl Model {
    fn of(steps: &[Step]) -> Self {
        let mut model = Self::default();
        for step in steps {
            match step {
                Step::Merk(prefix, puts, deletes) => {
                    model.subtrees.entry(*prefix).or_insert(false);
                    model.write(0, prefix, puts, deletes);
                }
                Step::Records(prefix, space, puts, deletes) => {
                    let flat = model.subtrees.entry(*prefix).or_insert(false);
                    *flat |= *space == Space::Flat;
                    model.write(space.column(), prefix, puts, deletes);
                }
                Step::Enter(prefix) => {
                    model.subtrees.entry(*prefix).or_insert(false);
                }
                Step::Clear(prefix) => model
                    .stored
                    .retain(|(column, key)| *column != 0 || !key.starts_with(prefix)),
                Step::Drop(prefix) => model.stored.retain(|(_, key)| !key.starts_with(prefix)),
                Step::Commit => {}
            }
        }
        model
    }

    fn write(&mut self, column: u8, prefix: &Prefix, puts: &[u64], deletes: &[u64]) {
        for key in puts {
            self.stored.insert((column, stored(prefix, &be(*key))));
        }
        for key in deletes {
            self.stored.remove(&(column, stored(prefix, &be(*key))));
        }
    }

    fn line(&self, space: Space) -> Line {
        let of_column = self
            .stored
            .iter()
            .filter(|(column, _)| *column == space.column());
        Line {
            keys: of_column.map(|(_, key)| key.clone()).collect(),
            at: None,
        }
    }
}

/// Where a cursor goes.
#[derive(Clone, Debug, PartialEq)]
enum Move {
    First,
    Last,
    From(Vec<u8>),
    Back(Vec<u8>),
    Next,
    Prev,
}

/// The stored keys of one column family in order, and the place of an
/// iterator among them.
struct Line {
    keys: Vec<Vec<u8>>,
    at: Option<usize>,
}

impl Line {
    fn seek(&mut self, key: &[u8]) {
        self.at = self.keys.iter().position(|stored| stored[..] >= *key);
    }

    fn seek_for_prev(&mut self, key: &[u8]) {
        self.at = self.keys.iter().rposition(|stored| stored[..] <= *key);
    }

    /// Makes the move of a cursor of the subtree `prefix`, and says which
    /// key of the subtree the cursor then stands on.
    fn go(&mut self, prefix: &Prefix, to: &Move) -> Option<Vec<u8>> {
        match to {
            Move::First => self.seek(prefix),
            Move::Last => {
                let mut after = *prefix;
                for byte in after.iter_mut().rev() {
                    *byte = byte.wrapping_add(1);
                    if *byte != 0 {
                        break;
                    }
                }
                self.seek_for_prev(&after);
            }
            Move::From(key) => self.seek(&stored(prefix, key)),
            Move::Back(key) => self.seek_for_prev(&stored(prefix, key)),
            // A step from nowhere goes nowhere.
            Move::Next => self.at = self.at.map(|at| at + 1).filter(|at| *at < self.keys.len()),
            Move::Prev => self.at = self.at.and_then(|at| at.checked_sub(1)),
        }
        let key = &self.keys[self.at?];
        key.strip_prefix(&prefix[..]).map(<[u8]>::to_vec)
    }
}

/// What a cursor shows after one move, and what the move and the looks
/// cost.
#[derive(Clone, Debug, PartialEq)]
struct Shown {
    valid: bool,
    key: Option<Vec<u8>>,
    value: Option<Vec<u8>>,
    cost: OperationCost,
}

fn walk<C: NodeCursor>(mut cursor: C, moves: &[Move]) -> Vec<Shown> {
    let shown = moves.iter().map(|to| {
        let mut cost = OperationCost::default();
        match to {
            Move::First => cursor.seek_to_first(),
            Move::Last => cursor.seek_to_last(),
            Move::From(key) => cursor.seek(key),
            Move::Back(key) => cursor.seek_for_prev(key),
            Move::Next => cursor.next(),
            Move::Prev => cursor.prev(),
        }
        .unwrap_add_cost(&mut cost);
        let valid = cursor.valid().unwrap_add_cost(&mut cost);
        let key = cursor.key().unwrap_add_cost(&mut cost).map(<[u8]>::to_vec);
        let value = (cursor.value().unwrap_add_cost(&mut cost)).map(<[u8]>::to_vec);
        cursor.status().expect("the cursor has not failed");
        Shown {
            valid,
            key,
            value,
            cost,
        }
    });
    shown.collect()
}

/// A seek past every key of a subtree, and one before every key.
fn past() -> Move {
    Move::From(vec![0xff; 9])
}

fn before() -> Move {
    Move::Back(Vec::new())
}

/// The walks of D-189 by name: each leaves its subtree and steps again.
fn named_walks() -> Vec<(&'static str, Vec<Move>)> {
    use Move::{First, Last, Next, Prev};
    vec![
        ("a seek past the end, then back", vec![past(), Prev, Prev]),
        (
            "a step past the last key, then back",
            vec![Last, Next, Prev, Prev],
        ),
        (
            "a seek before the first key, then on",
            vec![before(), Next, Next],
        ),
        (
            "a step before the first key, then on",
            vec![First, Prev, Next, Next],
        ),
        (
            "two steps on from past the end, then back",
            vec![past(), Next, Next, Prev, Prev, Prev, Prev],
        ),
        (
            "two steps back from before the first key, then on",
            vec![before(), Prev, Prev, Next, Next, Next, Next],
        ),
        ("a cursor that was never sought", vec![Next, Prev, Prev]),
        (
            "from past the end to before the first key and back",
            [vec![past()], vec![Prev; 6], vec![Next; 7]].concat(),
        ),
    ]
}

/// A walk the numbers of `seed` choose: mostly steps, so that it leaves
/// its subtree and goes on from there.
fn random_walk(seed: u64) -> Vec<Move> {
    let mut random = Random(seed | 1);
    let moves = (0..48).map(|_| match random.below(16) {
        0 => Move::First,
        1 => Move::Last,
        2 => past(),
        3 => before(),
        4 => Move::From(be(random.below(8))),
        5 => Move::Back(be(random.below(8))),
        6..=10 => Move::Next,
        _ => Move::Prev,
    });
    moves.collect()
}

fn walks() -> Vec<(String, Vec<Move>)> {
    let named = named_walks().into_iter();
    let named = named.map(|(name, moves)| (name.to_owned(), moves));
    let random = (0..24).map(|seed| (format!("the walk of seed {seed}"), random_walk(seed)));
    named.chain(random).collect()
}

/// What the model says a cursor of `space` of the subtree `prefix` shows
/// after each move: whether it stands on a key, and the key.
fn expected(model: &Model, space: Space, prefix: &Prefix, moves: &[Move]) -> Vec<Option<Vec<u8>>> {
    let mut line = model.line(space);
    moves.iter().map(|to| line.go(prefix, to)).collect()
}

/// The keys a cursor stood on after each of its moves. Every move is
/// charged as a move, whether or not it moved the cursor, and the looks
/// at the place are charged by the record of the subtree that is there:
/// a record of another subtree is charged as no record is.
fn places(shown: &[Shown]) -> Vec<Option<Vec<u8>>> {
    for at in shown {
        assert_eq!(at.valid, at.key.is_some());
        assert_eq!(at.valid, at.value.is_some());
        let stored = at.key.as_ref().map(|key| MODEL.stored_key_len(key.len()));
        let record = stored.zip(at.value.as_ref().map(Vec::len));
        let mut cost = MODEL.cursor_move();
        // `valid`, `key` and `value`.
        cost += MODEL.cursor_key(stored);
        cost += MODEL.cursor_key(stored);
        cost += MODEL.cursor_value(record);
        assert_eq!(at.cost, cost);
    }
    shown.iter().map(|at| at.key.clone()).collect()
}

fn node_batch(puts: &[u64], deletes: &[u64]) -> Vec<(Vec<u8>, Op)> {
    let puts = puts.iter().map(|key| basic(*key, &flat_value(&be(*key))));
    let deletes = deletes.iter().map(|key| (be(*key), Op::Delete));
    let mut batch: Vec<_> = puts.chain(deletes).collect();
    batch.sort_by(|a, b| a.0.cmp(&b.0));
    batch
}

/// The root key of each Merk of a grove, as its host would keep it.
type Roots = BTreeMap<Prefix, Option<Vec<u8>>>;

/// Makes `steps` on RocksDB, and returns the transaction that is open
/// after the last of them.
fn write_rocks<'a>(storage: &'a TempStorage, steps: &[Step]) -> RocksTransaction<'a> {
    let mut roots = Roots::new();
    let mut transaction = storage.start_transaction();
    for step in steps {
        match step {
            Step::Merk(prefix, ..) | Step::Clear(prefix) => {
                let staged = StorageBatch::new();
                let context = storage
                    .get_transactional_storage_context_by_subtree_prefix(
                        *prefix,
                        Some(&staged),
                        &transaction,
                    )
                    .unwrap();
                let root = roots.entry(*prefix).or_default();
                let mut merk = open_layered(context, root.clone());
                match step {
                    Step::Merk(_, puts, deletes) => {
                        apply(&mut merk, &node_batch(puts, deletes));
                    }
                    _ => merk.clear().unwrap().expect("the Merk is cleared"),
                }
                *root = merk.root_key();
                drop(merk);
                (storage.commit_multi_context_batch(staged, Some(&transaction)))
                    .unwrap()
                    .expect("the batch is applied");
            }
            Step::Records(prefix, space, puts, deletes) => {
                let context = storage
                    .get_immediate_storage_context_by_subtree_prefix(*prefix, &transaction)
                    .unwrap();
                write_records!(space, context, puts, deletes);
            }
            Step::Enter(_) => {}
            Step::Drop(prefix) => {
                let context = storage
                    .get_immediate_storage_context_by_subtree_prefix(*prefix, &transaction)
                    .unwrap();
                (FlatStore::clear(&context).unwrap()).expect("the data is removed");
                (context.aux().clear().unwrap()).expect("the aux records are removed");
                (context.meta().clear().unwrap()).expect("the meta records are removed");
                roots.remove(prefix);
            }
            Step::Commit => {
                (storage.commit_transaction(transaction).unwrap()).expect("a commit");
                transaction = storage.start_transaction();
            }
        }
    }
    transaction
}

/// What a cursor of `space` of each subtree shows on each walk, by the
/// subtree and the name of the walk.
type Answers = BTreeMap<(Prefix, Space, String), Vec<Shown>>;

/// The subtrees and the spaces a test reads: every space of every subtree,
/// but not the flat data of a subtree that holds a Merk. A node cursor
/// over a tree that keeps its data flat shows the flat records, as
/// RocksDB's does (D-197). The flat cursor of a Merk's subtree does not
/// show the nodes.
fn readings(model: &Model) -> Vec<(Prefix, Space)> {
    let mut readings = Vec::new();
    // Three subtrees that were never entered: before every other, among
    // them and behind them.
    for prefix in NEVER_ENTERED {
        assert!(!model.subtrees.contains_key(&prefix));
        readings.extend(SPACES.map(|space| (prefix, space)));
    }
    for (prefix, flat) in &model.subtrees {
        let holds_data = model
            .stored
            .iter()
            .any(|(column, key)| *column == 0 && key.starts_with(prefix));
        for space in SPACES {
            let other = space == Space::Flat && holds_data && !*flat;
            if !other {
                readings.push((*prefix, space));
            }
        }
    }
    readings
}

/// How a handle of RocksDB is made.
#[derive(Clone, Copy, Debug)]
enum RocksHandle {
    Deferred,
    Immediate,
}

fn rocks_answers(
    storage: &TempStorage,
    transaction: &RocksTransaction<'_>,
    handle: RocksHandle,
    model: &Model,
) -> Answers {
    let mut answers = Answers::new();
    for (prefix, space) in readings(model) {
        for (name, moves) in walks() {
            let shown = match handle {
                RocksHandle::Deferred => {
                    let context = storage
                        .get_transactional_storage_context_by_subtree_prefix(
                            prefix,
                            None,
                            transaction,
                        )
                        .unwrap();
                    with_cursor!(space, context, |cursor| walk(cursor, &moves))
                }
                RocksHandle::Immediate => {
                    let context = storage
                        .get_immediate_storage_context_by_subtree_prefix(prefix, transaction)
                        .unwrap();
                    with_cursor!(space, context, |cursor| walk(cursor, &moves))
                }
            };
            assert_eq!(
                places(&shown),
                expected(model, space, &prefix, &moves),
                "RocksDB and the model, {space:?} of {:?}, {name}: {moves:?}",
                prefix[0]
            );
            answers.insert((prefix, space, name), shown);
        }
    }
    answers
}

/// Makes `steps` on RocksDB and holds every cursor of the seam to the
/// model: in the transaction that made the last steps, in the next one,
/// and in a snapshot read transaction. Returns what the cursors showed in
/// the first and in the last.
fn on_rocksdb(steps: &[Step]) -> (Answers, Answers) {
    let model = Model::of(steps);
    let storage = &TempStorage::new();
    let transaction = write_rocks(storage, steps);
    let open = rocks_answers(storage, &transaction, RocksHandle::Deferred, &model);
    let immediate = rocks_answers(storage, &transaction, RocksHandle::Immediate, &model);
    assert_eq!(open, immediate, "a handle that writes at once");
    (storage.commit_transaction(transaction).unwrap()).expect("a commit");

    let transaction = storage.start_transaction();
    let next = rocks_answers(storage, &transaction, RocksHandle::Deferred, &model);
    assert_eq!(open, next, "the next transaction");
    drop(transaction);
    let transaction = storage.start_snapshot_read_transaction();
    let snapshot = rocks_answers(storage, &transaction, RocksHandle::Deferred, &model);
    assert_eq!(open, snapshot, "a snapshot");
    (open, snapshot)
}

const NEVER_ENTERED: [Prefix; 3] = [[0x10; 32], [0x50; 32], [0xf0; 32]];
const P1: Prefix = [0x20; 32];
const P2: Prefix = [0x40; 32];
const P3: Prefix = [0x60; 32];
const P4: Prefix = [0x80; 32];

/// Nodes, aux records and meta records of a subtree.
fn merk_with_side_records(prefix: Prefix, keys: &[u64]) -> Vec<Step> {
    vec![
        Step::Merk(prefix, keys.to_vec(), Vec::new()),
        Step::Records(prefix, Space::Aux, keys.to_vec(), Vec::new()),
        Step::Records(prefix, Space::Meta, keys.to_vec(), Vec::new()),
    ]
}

/// Flat data, aux records and meta records of a subtree.
fn flat_with_side_records(prefix: Prefix, keys: &[u64]) -> Vec<Step> {
    vec![
        Step::Records(prefix, Space::Flat, keys.to_vec(), Vec::new()),
        Step::Records(prefix, Space::Aux, keys.to_vec(), Vec::new()),
        Step::Records(prefix, Space::Meta, keys.to_vec(), Vec::new()),
    ]
}

/// The groves of these tests, by name. Each is read in the transaction
/// that made the steps after its last commit, and again once those are
/// committed.
fn groves() -> Vec<(&'static str, Vec<Step>)> {
    let three = || {
        [
            merk_with_side_records(P1, &[1, 2]),
            merk_with_side_records(P2, &[1, 2, 3]),
            merk_with_side_records(P3, &[4]),
        ]
        .concat()
    };
    let committed = |mut steps: Vec<Step>| {
        steps.push(Step::Commit);
        steps
    };
    vec![
        ("three Merks, committed", committed(three())),
        ("three Merks, written by the transaction", three()),
        (
            "a Merk alone",
            committed(merk_with_side_records(P2, &[1, 2, 3])),
        ),
        (
            "a Merk between two subtrees that are entered and hold nothing",
            committed(
                [
                    vec![Step::Enter(P1)],
                    merk_with_side_records(P2, &[1, 2, 3]),
                    vec![Step::Enter(P3)],
                ]
                .concat(),
            ),
        ),
        (
            "a Merk between two trees that keep their data flat",
            committed(
                [
                    flat_with_side_records(P1, &[5, 6]),
                    merk_with_side_records(P2, &[1, 2, 3]),
                    flat_with_side_records(P3, &[7]),
                ]
                .concat(),
            ),
        ),
        (
            "a subtree that holds nothing between two Merks",
            committed(
                [
                    merk_with_side_records(P1, &[1, 2]),
                    vec![Step::Enter(P2)],
                    merk_with_side_records(P3, &[4, 5]),
                ]
                .concat(),
            ),
        ),
        (
            "an empty subtree between a Merk and the next that holds records",
            committed(
                [
                    merk_with_side_records(P1, &[1]),
                    merk_with_side_records(P2, &[1, 2, 3]),
                    vec![Step::Enter(P3)],
                    flat_with_side_records(P4, &[1, 2]),
                ]
                .concat(),
            ),
        ),
        (
            "a Merk whose neighbours the transaction writes",
            [
                committed(merk_with_side_records(P2, &[1, 2, 3])),
                merk_with_side_records(P1, &[1, 2]),
                flat_with_side_records(P3, &[7, 8]),
            ]
            .concat(),
        ),
        (
            "neighbours with committed nodes that the transaction writes",
            [
                committed(three()),
                vec![
                    Step::Merk(P3, vec![5, 6], Vec::new()),
                    Step::Merk(P1, vec![3], vec![1]),
                ],
            ]
            .concat(),
        ),
        (
            "a neighbour the transaction drops, and one it clears",
            [committed(three()), vec![Step::Drop(P3), Step::Clear(P1)]].concat(),
        ),
        (
            "a neighbour the transaction empties key by key",
            [
                committed(three()),
                vec![
                    Step::Merk(P3, Vec::new(), vec![4]),
                    Step::Records(P3, Space::Aux, Vec::new(), vec![4]),
                    Step::Records(P1, Space::Meta, Vec::new(), vec![1, 2]),
                ],
            ]
            .concat(),
        ),
        (
            "a neighbour that was dropped and is entered again with nothing",
            [
                committed(three()),
                committed(vec![Step::Drop(P3)]),
                vec![Step::Enter(P3), Step::Drop(P1), Step::Enter(P1)],
            ]
            .concat(),
        ),
        (
            "a neighbour that was dropped and is written again",
            [
                committed(three()),
                vec![Step::Drop(P3)],
                flat_with_side_records(P3, &[2, 3]),
            ]
            .concat(),
        ),
        (
            "the own subtree emptied by the transaction",
            [
                committed(three()),
                vec![
                    Step::Clear(P2),
                    Step::Records(P2, Space::Aux, Vec::new(), vec![1, 2, 3]),
                    Step::Records(P2, Space::Meta, Vec::new(), vec![1, 2, 3]),
                ],
            ]
            .concat(),
        ),
    ]
}

/// D-189, the reference: on RocksDB a cursor of every keyspace, in every
/// kind of transaction, stands where the line of its column family says,
/// and a step from no key goes on from there.
#[test]
fn rocksdb_steps_from_no_key_along_the_line_of_its_column_family() {
    let step_back = "a seek past the end, then back".to_owned();
    for (name, steps) in groves() {
        let (open, _) = on_rocksdb(&steps);
        if name != "three Merks, committed" {
            continue;
        }
        // The defect of D-189, in the grove of three Merks: the seek goes
        // past the end of each, and the step back comes onto the last key
        // of the first two and goes nowhere in the last.
        for (prefix, last) in [(P1, Some(be(2))), (P2, Some(be(3))), (P3, None)] {
            let shown = &open[&(prefix, Space::Nodes, step_back.clone())];
            assert_eq!((shown[0].valid, &shown[1].key), (false, &last));
        }
    }
}

/// A database of the engine's facade.
struct Facade {
    _dir: tempfile::TempDir,
    db: Database,
}

impl Facade {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("a directory");
        let db = Database::open(dir.path(), &Options::default()).expect("a database");
        Self { _dir: dir, db }
    }

    fn commit(&self, transaction: &WriteTxn<'_>) {
        let committed = self.db.commit(transaction, &mut NoStaleHashes);
        assert!(
            matches!(committed, Ok(Committed::Published(_) | Committed::Empty)),
            "{committed:?}"
        );
    }
}

/// Makes `steps` on LigninDB, and returns the transaction that is open
/// after the last of them.
fn write_lignin<'a>(facade: &'a Facade, steps: &[Step]) -> WriteTxn<'a> {
    let mut roots = Roots::new();
    let mut entered = BTreeSet::new();
    let mut transaction = facade.db.begin();
    for step in steps {
        match step {
            Step::Merk(prefix, ..) | Step::Clear(prefix) => {
                let staged = LigninBatch::new();
                let subtree = LigninSubtree::alone(*prefix);
                let store = LigninNodeStore::deferred(&transaction, subtree, Some(&staged));
                if entered.insert(*prefix) {
                    (store.enter(TreeType::NormalTree)).expect("the subtree is entered");
                }
                let root = roots.entry(*prefix).or_default();
                let mut merk = open_layered(store, root.clone());
                match step {
                    Step::Merk(_, puts, deletes) => {
                        apply(&mut merk, &node_batch(puts, deletes));
                    }
                    _ => merk.clear().unwrap().expect("the Merk is cleared"),
                }
                *root = merk.root_key();
                drop(merk);
                (staged.apply(&transaction).unwrap()).expect("the batch is applied");
            }
            Step::Records(prefix, space, puts, deletes) => {
                let subtree = LigninSubtree::alone(*prefix);
                let handle = LigninNodeStore::immediate(&transaction, subtree);
                if entered.insert(*prefix) {
                    let tree_type = match space {
                        Space::Flat => TreeType::MmrTree,
                        _ => TreeType::NormalTree,
                    };
                    handle.enter(tree_type).expect("the subtree is entered");
                }
                write_records!(space, handle, puts, deletes);
            }
            Step::Enter(prefix) => {
                let subtree = LigninSubtree::alone(*prefix);
                let handle = LigninNodeStore::immediate(&transaction, subtree);
                assert!(entered.insert(*prefix));
                (handle.enter(TreeType::NormalTree)).expect("the subtree is entered");
            }
            Step::Drop(prefix) => {
                let staged = LigninBatch::new();
                staged.drop_subtree(&LigninSubtree::alone(*prefix));
                (staged.apply(&transaction).unwrap()).expect("the subtree is dropped");
                entered.remove(prefix);
                roots.remove(prefix);
            }
            Step::Commit => {
                facade.commit(&transaction);
                transaction = facade.db.begin();
            }
        }
    }
    transaction
}

/// What the cursors of handles on `view` show, on every walk.
fn lignin_answers<'db>(view: impl Into<LigninView<'db>> + Copy, model: &Model) -> Answers {
    let mut answers = Answers::new();
    for (prefix, space) in readings(model) {
        for (name, moves) in walks() {
            let handle = LigninNodeStore::read_only(view, LigninSubtree::alone(prefix));
            let shown = with_cursor!(space, handle, |cursor| walk(cursor, &moves));
            answers.insert((prefix, space, name), shown);
        }
    }
    answers
}

fn assert_same_answers(found: &Answers, expected: &Answers, grove: &str, view: &str) {
    assert_eq!(found.len(), expected.len());
    for ((prefix, space, name), expected) in expected {
        let found = &found[&(*prefix, *space, name.clone())];
        for (at, (found, expected)) in found.iter().zip(expected).enumerate() {
            assert_eq!(
                found, expected,
                "{grove}, {view}: {space:?} of {:?}, {name}, after move {at}",
                prefix[0]
            );
        }
    }
}

/// D-189: a cursor of the store stands where the cursor of RocksDB stands
/// after every move, a step from no key among them, shows the same record
/// and is charged the same. So it is for the nodes of a Merk and for each
/// flat keyspace; through a transaction, through a writer's scope and
/// through a snapshot; with the subtree first, in the middle and last in
/// the order of the prefixes; and with neighbours that are committed,
/// written by the transaction, entered and empty, never entered, dropped,
/// cleared, and flat.
#[test]
fn a_step_from_no_key_goes_where_it_goes_on_rocksdb() {
    for (name, steps) in groves() {
        let model = Model::of(&steps);
        let (open, committed) = on_rocksdb(&steps);
        let facade = &Facade::new();
        let transaction = write_lignin(facade, &steps);

        let found = lignin_answers(&transaction, &model);
        assert_same_answers(&found, &open, name, "the transaction");
        let writer = LigninWriter::new(transaction.scope().expect("a scope"));
        let found = lignin_answers(&writer, &model);
        assert_same_answers(&found, &open, name, "a writer's scope");
        writer.into_scope().merge();
        facade.commit(&transaction);

        let transaction = facade.db.begin();
        let found = lignin_answers(&transaction, &model);
        assert_same_answers(&found, &committed, name, "the next transaction");
        let snapshot = LigninSnapshot::new(facade.db.snapshot());
        let found = lignin_answers(&snapshot, &model);
        assert_same_answers(&found, &committed, name, "a snapshot");
    }
}

/// The keys of the rows `query` answers through `cursor`, and what the
/// read costs.
fn keys_of<C: NodeCursor>(cursor: C, query: &Query) -> (Vec<Vec<u8>>, OperationCost) {
    let mut cost = OperationCost::default();
    let mut rows = KVIterator::new(cursor, query).unwrap_add_cost(&mut cost);
    let mut found = Vec::new();
    while let Some((key, _)) = rows.next_kv().unwrap_add_cost(&mut cost) {
        found.push(key);
    }
    rows.status().expect("the scan has not failed");
    (found, cost)
}

/// The read D-189 began with: a descending read of a range whose end is
/// past the last key of the Merk, which seeks to the end and steps back.
/// It answers the rows where a record of another subtree follows, and
/// nothing where none does, on both engines.
#[test]
fn a_descending_range_past_the_last_key_answers_as_on_rocksdb() {
    let steps = [
        merk_with_side_records(P1, &[1, 2]),
        merk_with_side_records(P2, &[1, 2, 3]),
        merk_with_side_records(P3, &[4, 5]),
        vec![Step::Commit],
    ]
    .concat();
    let queries = || {
        let range = QueryItem::Range(be(2)..vec![0xff; 9]);
        let range_to = QueryItem::RangeTo(..vec![0xff; 9]);
        let range_after_to = QueryItem::RangeAfterTo(be(1)..vec![0xff; 9]);
        let mut queries = Vec::new();
        for item in [range, range_to, range_after_to] {
            for left_to_right in [false, true] {
                let mut query = Query::new_with_direction(left_to_right);
                query.insert_item(item.clone());
                queries.push(query);
            }
        }
        queries
    };
    let storage = &TempStorage::new();
    let rocks_transaction = write_rocks(storage, &steps);
    let facade = &Facade::new();
    let transaction = write_lignin(facade, &steps);
    let snapshot = LigninSnapshot::new(facade.db.snapshot());
    for (prefix, keys) in [(P1, 2), (P2, 3), (P3, 2)] {
        let context = storage
            .get_transactional_storage_context_by_subtree_prefix(prefix, None, &rocks_transaction)
            .unwrap();
        for query in queries() {
            let expected = keys_of(NodeStore::cursor(&context), &query);
            let at = format!("{:?} of {:?}", query.items, prefix[0]);
            match (query.left_to_right, prefix == P3) {
                // The last Merk of the column family, read from its end:
                // the step from past the end goes nowhere, and the read
                // answers nothing.
                (false, true) => assert_eq!(expected.0.len(), 0, "{at}"),
                _ => assert!(expected.0.len() >= keys - 1, "{at}"),
            }
            let subtree = LigninSubtree::alone(prefix);
            let handle = LigninNodeStore::read_only(&transaction, subtree.clone());
            let found = keys_of(NodeStore::cursor(&handle), &query);
            assert_eq!(found, expected, "{at}");
            let handle = LigninNodeStore::read_only(&snapshot, subtree);
            let found = keys_of(NodeStore::cursor(&handle), &query);
            assert_eq!(found, expected, "{at}, in a snapshot");
        }
    }
}

/// The key and the record of every node of a Merk of the keys `keys`, as
/// RocksDB stores them, with the key of its root.
fn merk_records(keys: std::ops::Range<u64>) -> (Vec<(Vec<u8>, Vec<u8>)>, Vec<u8>) {
    let storage = TempStorage::new();
    let transaction = write_rocks(&storage, &[Step::Merk(P1, keys.collect(), Vec::new())]);
    let context = storage
        .get_transactional_storage_context_by_subtree_prefix(P1, None, &transaction)
        .unwrap();
    let mut cursor = NodeStore::cursor(&context);
    let mut stored = Vec::new();
    cursor.seek_to_first().unwrap();
    while let Some(key) = cursor.key().unwrap().map(<[u8]>::to_vec) {
        let record = cursor.value().unwrap().expect("a record").to_vec();
        stored.push((key, record));
        cursor.next().unwrap();
    }
    // The root is the node that no other names.
    let linked = |candidate: &Vec<u8>| {
        stored.iter().any(|(_, record)| {
            let node = TreeNode::decode_raw_for_read(record).expect("a node");
            let links = [node.left, node.right];
            (links.iter().flatten()).any(|link| link.key() == candidate.as_slice())
        })
    };
    let root = stored.iter().map(|(key, _)| key).find(|key| !linked(key));
    let root = root.cloned();
    (stored, root.expect("a root"))
}

/// What the reads of a node by its key answer for `key`, and what each is
/// charged.
fn node_reads<'db, S: NodeStore<'db>>(store: &S, key: &[u8]) -> Vec<(String, OperationCost)> {
    let got = store.get_node(key, NO_VALUE_DEFINED_COST, version());
    let got = (
        format!("{:?}", got.value.map(|node| node.map(|node| node.encode()))),
        got.cost,
    );
    let for_read = store.get_node_for_read(key);
    let for_read = (
        format!(
            "{:?}",
            for_read.value.map(|node| node.map(|node| {
                let links =
                    [node.left, node.right].map(|link| link.map(|link| link.key().to_vec()));
                (node.kv.value_as_slice().to_vec(), links)
            }))
        ),
        for_read.cost,
    );
    let has = store.has_node(key);
    let has = (format!("{:?}", has.value), has.cost);
    vec![got, for_read, has]
}

/// D-197: a subtree of a tree that keeps its data as flat records holds
/// no Merk, and RocksDB reads a node there all the same, by its key and
/// under a cursor: the flat record, with its bytes. The store answers the
/// same and charges the same, for records that are in the form of Merk's
/// nodes, as a state sync can be asked for, and for records that are not:
/// in the transaction that put them, in the next, in a writer's scope and
/// in a snapshot. A Merk that is opened there with the key of a record as
/// its root is read whole on both.
#[test]
fn a_flat_record_is_read_as_a_node_where_its_tree_keeps_flat_records() {
    let (nodes, root) = merk_records(0..7);
    assert_eq!(nodes.len(), 7);
    let other: [(Vec<u8>, Vec<u8>); 3] = [
        (b"empty".to_vec(), Vec::new()),
        (b"short".to_vec(), vec![1]),
        (b"some bytes".to_vec(), vec![0x5a; 77]),
    ];
    let records: Vec<_> = nodes.iter().chain(&other).cloned().collect();
    let absent = b"no record".to_vec();
    let keys = || records.iter().map(|(key, _)| key).chain([&absent]);
    for tree_type in [
        TreeType::MmrTree,
        TreeType::BulkAppendTree(3),
        TreeType::DenseAppendOnlyFixedSizeTree(3),
        TreeType::CommitmentTree(3),
        TreeType::PrivateDocumentStore(3),
    ] {
        let storage = &TempStorage::new();
        let rocks_transaction = storage.start_transaction();
        let context = storage
            .get_immediate_storage_context_by_subtree_prefix(P2, &rocks_transaction)
            .unwrap();
        let facade = &Facade::new();
        let transaction = facade.db.begin();
        let handle = LigninNodeStore::immediate(&transaction, LigninSubtree::alone(P2));
        handle.enter(tree_type).expect("the subtree is entered");
        for (key, record) in &records {
            (FlatStore::put(&context, key, record, None).unwrap()).expect("a put");
            (FlatStore::put(&handle, key, record, None).unwrap()).expect("a put");
        }

        // What RocksDB answers, by key and for a Merk with the root's
        // key: through a context that writes at once, whose reads are
        // charged nothing, and through one that stages, whose reads are
        // charged.
        let deferred = storage
            .get_transactional_storage_context_by_subtree_prefix(P2, None, &rocks_transaction)
            .unwrap();
        let at_once: Vec<_> = keys().map(|key| node_reads(&context, key)).collect();
        let charged: Vec<_> = keys().map(|key| node_reads(&deferred, key)).collect();
        let decoded = |reads: &[(String, OperationCost)]| reads[0].0.starts_with("Ok(Some(");
        let refused = |reads: &[(String, OperationCost)]| reads[0].0.starts_with("Err(");
        for expected in [&at_once, &charged] {
            assert_eq!(expected.iter().filter(|reads| decoded(reads)).count(), 7);
            assert_eq!(expected.iter().filter(|reads| refused(reads)).count(), 3);
        }
        let loaded = |reads: &[(String, OperationCost)]| reads[0].1.storage_loaded_bytes;
        assert!(at_once.iter().all(|reads| loaded(reads) == 0));
        // Also a read of a record that does not decode is charged its
        // bytes.
        assert_eq!(charged.iter().filter(|reads| loaded(reads) > 0).count(), 9);
        assert_eq!(super::records(&context, Some(root.clone())), nodes);

        let moves = [vec![Move::First], vec![Move::Next; records.len()]].concat();
        let there = walk(NodeStore::cursor(&context), &moves);
        let check = |store: &LigninNodeStore<'_>, expected: &Vec<_>, view: &str| {
            for (key, expected) in keys().zip(expected) {
                let found = node_reads(store, key);
                assert_eq!(&found, expected, "{tree_type:?}, {view}: {key:?}");
            }
            assert_eq!(super::records(store, Some(root.clone())), nodes, "{view}");
            // The node cursor shows every record with its bytes.
            let found = walk(NodeStore::cursor(store), &moves);
            assert_eq!(found, there, "{tree_type:?}, {view}: the cursor");
            let shown = found.iter().filter(|shown| shown.value.is_some()).count();
            assert_eq!(shown, records.len());
        };
        let subtree = || LigninSubtree::alone(P2);
        check(&handle, &at_once, "the handle that put them");
        drop(handle);
        check(
            &LigninNodeStore::read_only(&transaction, subtree()),
            &charged,
            "the transaction that put them",
        );
        facade.commit(&transaction);

        let transaction = facade.db.begin();
        check(
            &LigninNodeStore::read_only(&transaction, subtree()),
            &charged,
            "the next transaction",
        );
        let writer = LigninWriter::new(transaction.scope().expect("a scope"));
        check(
            &LigninNodeStore::read_only(&writer, subtree()),
            &charged,
            "a writer's scope",
        );
        writer.into_scope().merge();
        let snapshot = LigninSnapshot::new(facade.db.snapshot());
        check(
            &LigninNodeStore::read_only(&snapshot, subtree()),
            &charged,
            "a snapshot",
        );
    }
}

/// A cursor reads the state it was opened on, and a flat record with it:
/// the record under the cursor's key is read by the tree type that the
/// cursor's own version has the subtree entered with, whatever is
/// committed beside the cursor. Here the subtree is dropped by a commit
/// between the seek and the read of the value. RocksDB's iterator shows
/// the record it stands on, and the store's cursor shows the same, at the
/// same charges. A read by key through the same handle reads the latest
/// committed state on both, which has no record.
#[test]
fn a_cursor_reads_a_flat_record_by_the_tree_type_of_its_own_version() {
    let (key, value) = (b"a".to_vec(), vec![0x5a]);
    let moves = [Move::First];

    let storage = &TempStorage::new();
    let rocks_transaction = storage.start_transaction();
    {
        let context = storage
            .get_immediate_storage_context_by_subtree_prefix(P2, &rocks_transaction)
            .unwrap();
        (FlatStore::put(&context, &key, &value, None).unwrap()).expect("a put");
    }
    (storage.commit_transaction(rocks_transaction).unwrap()).expect("a commit");
    let reader = storage.start_transaction();
    let context = storage
        .get_transactional_storage_context_by_subtree_prefix(P2, None, &reader)
        .unwrap();
    let mut on_rocks = NodeStore::cursor(&context);
    on_rocks.seek_to_first().unwrap();
    let rocks_transaction = storage.start_transaction();
    {
        let other = storage
            .get_immediate_storage_context_by_subtree_prefix(P2, &rocks_transaction)
            .unwrap();
        (FlatStore::clear(&other).unwrap()).expect("the data is removed");
    }
    (storage.commit_transaction(rocks_transaction).unwrap()).expect("a commit");
    type Read = (bool, Option<Vec<u8>>, Option<Vec<u8>>, bool, OperationCost);
    fn read<C: NodeCursor>(cursor: &mut C) -> Read {
        let mut cost = OperationCost::default();
        let valid = cursor.valid().unwrap_add_cost(&mut cost);
        let at = cursor.key().unwrap_add_cost(&mut cost).map(<[u8]>::to_vec);
        let shown = (cursor.value().unwrap_add_cost(&mut cost)).map(<[u8]>::to_vec);
        (valid, at, shown, cursor.status().is_ok(), cost)
    }
    let there = read(&mut on_rocks);
    assert!(there.0 && there.3);
    assert_eq!(
        (&there.1, &there.2),
        (&Some(key.clone()), &Some(value.clone()))
    );
    let gone_there = node_reads(&context, &key);

    let facade = &Facade::new();
    let transaction = facade.db.begin();
    let handle = LigninNodeStore::immediate(&transaction, LigninSubtree::alone(P2));
    (handle.enter(TreeType::MmrTree)).expect("the subtree is entered");
    (FlatStore::put(&handle, &key, &value, None).unwrap()).expect("a put");
    drop(handle);
    facade.commit(&transaction);
    let reader = facade.db.begin();
    let store = LigninNodeStore::read_only(&reader, LigninSubtree::alone(P2));
    let mut on_lignin = NodeStore::cursor(&store);
    on_lignin.seek_to_first().unwrap();
    let transaction = write_lignin(facade, &[Step::Drop(P2), Step::Commit]);
    drop(transaction);
    assert_eq!(read(&mut on_lignin), there);
    assert_eq!(node_reads(&store, &key), gone_there);
    // A cursor that is opened now shows no record on either.
    assert_eq!(
        walk(NodeStore::cursor(&store), &moves),
        walk(NodeStore::cursor(&context), &moves)
    );
}

/// A flat record of a tree that keeps flat records, in the form of a
/// node's record, is read as that node, and the node can be written back
/// through a change set of the handle that read it with a change that
/// keeps its value (`NodeChange::HashOnly`): no cell keeps a value for
/// the node, so the store writes the value that the node holds. RocksDB
/// writes the whole record with every write. The handle then reads the
/// node on both, and on LigninDB the key holds another record than the
/// one that was put: the node was written.
#[test]
fn a_node_that_was_read_from_a_flat_record_is_written_back_with_its_value() {
    fn rewrite<'db, S: NodeStore<'db>>(store: &S, key: &[u8]) -> Vec<String> {
        let node = store.get_node(key, NO_VALUE_DEFINED_COST, version());
        let node = node.unwrap().expect("a read").expect("a node");
        let mut change_set = store.change_set();
        let prepared = change_set.prepare_node(&node, NodeChange::HashOnly);
        change_set.put_node(key, prepared, None).unwrap();
        store.commit(change_set).unwrap().expect("a commit");
        let reads = node_reads(store, key);
        reads.into_iter().map(|(answer, _)| answer).collect()
    }
    let (nodes, _) = merk_records(0..1);
    let (key, record) = &nodes[0];

    let storage = &TempStorage::new();
    let rocks_transaction = storage.start_transaction();
    let context = storage
        .get_immediate_storage_context_by_subtree_prefix(P2, &rocks_transaction)
        .unwrap();
    (FlatStore::put(&context, key, record, None).unwrap()).expect("a put");
    let there = rewrite(&context, key);
    assert!(there[0].starts_with("Ok(Some("), "{there:?}");

    let facade = &Facade::new();
    let transaction = facade.db.begin();
    let handle = LigninNodeStore::immediate(&transaction, LigninSubtree::alone(P2));
    (handle.enter(TreeType::MmrTree)).expect("the subtree is entered");
    (FlatStore::put(&handle, key, record, None).unwrap()).expect("a put");
    drop(handle);
    facade.commit(&transaction);
    let transaction = facade.db.begin();
    let handle = LigninNodeStore::immediate(&transaction, LigninSubtree::alone(P2));
    assert_eq!(rewrite(&handle, key), there);
    drop(handle);
    facade.commit(&transaction);
    let reader = facade.db.begin();
    let store = LigninNodeStore::read_only(&reader, LigninSubtree::alone(P2));
    let stored = FlatStore::get(&store, key).unwrap().expect("a read");
    let stored = stored.expect("the key holds a record");
    assert_ne!(&stored, record, "the node was written");
}

/// A delete of a node that states no cost is charged from the committed
/// record under the key (`cost-model.md` D2). Here that record is a flat
/// record that was put with 21 bytes under a tree that keeps flat
/// records. It is read with the bytes it is stored with, by the tree type
/// that the committed state has the subtree entered with: the batch is
/// charged what RocksDB charges it, and the record is gone on both.
#[test]
fn a_delete_with_no_cost_of_a_flat_record_is_charged_from_its_stored_bytes() {
    fn delete<'db, S: NodeStore<'db>>(store: &S, key: &[u8]) {
        let mut change_set = store.change_set();
        change_set.delete_node(key, None);
        store
            .commit(change_set)
            .unwrap()
            .expect("the delete is staged");
    }
    let (key, value) = (b"k".to_vec(), vec![0x5a; 21]);

    let storage = &TempStorage::new();
    let rocks_transaction = storage.start_transaction();
    {
        let context = storage
            .get_immediate_storage_context_by_subtree_prefix(P2, &rocks_transaction)
            .unwrap();
        (FlatStore::put(&context, &key, &value, None).unwrap()).expect("a put");
    }
    (storage.commit_transaction(rocks_transaction).unwrap()).expect("a commit");
    let rocks_transaction = storage.start_transaction();
    let staged = StorageBatch::new();
    {
        let context = storage
            .get_transactional_storage_context_by_subtree_prefix(
                P2,
                Some(&staged),
                &rocks_transaction,
            )
            .unwrap();
        delete(&context, &key);
    }
    let there = storage.commit_multi_context_batch(staged, Some(&rocks_transaction));
    there.value.expect("the batch is applied on RocksDB");
    assert!(there.cost.storage_cost.removed_bytes.total_removed_bytes() > 0);
    assert_eq!(there.cost.storage_loaded_bytes, 21);
    let context = storage
        .get_transactional_storage_context_by_subtree_prefix(P2, None, &rocks_transaction)
        .unwrap();
    let gone_there = node_reads(&context, &key);
    assert_eq!(gone_there[2].0, "Ok(false)");

    let facade = &Facade::new();
    let transaction = facade.db.begin();
    let handle = LigninNodeStore::immediate(&transaction, LigninSubtree::alone(P2));
    (handle.enter(TreeType::MmrTree)).expect("the subtree is entered");
    (FlatStore::put(&handle, &key, &value, None).unwrap()).expect("a put");
    drop(handle);
    facade.commit(&transaction);
    let transaction = facade.db.begin();
    let staged = LigninBatch::new();
    let subtree = LigninSubtree::alone(P2);
    let store = LigninNodeStore::deferred(&transaction, subtree, Some(&staged));
    delete(&store, &key);
    drop(store);
    let here = staged.apply(&transaction);
    here.value.expect("the batch is applied on LigninDB");
    assert_eq!(here.cost, there.cost);
    let store = LigninNodeStore::read_only(&transaction, LigninSubtree::alone(P2));
    assert_eq!(node_reads(&store, &key), gone_there);
}

/// Writes the Merk of `P2` through `transaction`: puts of the first keys
/// and deletes of the others, from the root `root`. Returns the root that
/// is left.
fn merk_of_p2(
    transaction: &WriteTxn<'_>,
    root: Option<Vec<u8>>,
    puts: &[u64],
    deletes: &[u64],
) -> Option<Vec<u8>> {
    merk_batch_of_p2(transaction, root, &node_batch(puts, deletes))
}

/// Applies `batch` to the Merk of `P2` through `transaction`, from the
/// root `root`. Returns the root that is left.
fn merk_batch_of_p2(
    transaction: &WriteTxn<'_>,
    root: Option<Vec<u8>>,
    batch: &[(Vec<u8>, Op)],
) -> Option<Vec<u8>> {
    let staged = LigninBatch::new();
    let subtree = LigninSubtree::alone(P2);
    let store = LigninNodeStore::deferred(transaction, subtree, Some(&staged));
    let mut merk = open_layered(store, root);
    apply(&mut merk, batch);
    let root = merk.root_key();
    drop(merk);
    (staged.apply(transaction).unwrap()).expect("the batch is applied");
    root
}

/// A read that comes to a flat record is no read of a cell, and changes
/// nothing of what the handle remembers of the cells it read under the
/// key. Here a handle that has read nothing else under the key sees a
/// flat record there that is no node, and reads a node of a Merk's tree
/// there after the commits of other transactions. It writes that node
/// back with a change that keeps its value: the keep names the cell it
/// read.
#[test]
fn a_handle_that_saw_a_flat_record_keeps_a_value_by_the_cell_it_reads_later() {
    let key = be(1);
    let facade = &Facade::new();
    let transaction = facade.db.begin();
    let handle = LigninNodeStore::immediate(&transaction, LigninSubtree::alone(P2));
    (handle.enter(TreeType::MmrTree)).expect("the subtree is entered");
    (FlatStore::put(&handle, &key, &[0x5a], None).unwrap()).expect("a put");
    drop(handle);
    facade.commit(&transaction);

    let first = facade.db.begin();
    let handle = LigninNodeStore::immediate(&first, LigninSubtree::alone(P2));
    let no_node = handle.get_node(&key, NO_VALUE_DEFINED_COST, version());
    no_node.value.expect_err("the record is no node");

    // Two commits beside the handle: the record is removed, and a Merk
    // is written under the prefix with a node under the key.
    let other = facade.db.begin();
    let flat = LigninNodeStore::immediate(&other, LigninSubtree::alone(P2));
    (FlatStore::delete(&flat, &key, None).unwrap()).expect("the record is removed");
    drop(flat);
    facade.commit(&other);
    let other = facade.db.begin();
    assert_eq!(merk_of_p2(&other, None, &[1], &[]), Some(key.clone()));
    facade.commit(&other);

    let node = handle.get_node(&key, NO_VALUE_DEFINED_COST, version());
    let node = node
        .value
        .expect("a read")
        .expect("the node of the other commit");
    let kept = handle.write_node(&node, NodeChange::HashOnly);
    kept.value.expect("a keep by the cell that was read");
}

/// The other way round: a handle read a node of a Merk's tree, and
/// commits beside it take the node and put a flat record under its key.
/// The handle's next read comes to the flat record, which is no node. A
/// write of the node it read before, with a change that keeps its value,
/// still names the cell that is gone: it does not go through as a write
/// that states the value, and the record of the other commit stays.
#[test]
fn a_flat_record_under_a_key_does_not_free_a_keep_from_the_cell_it_was_read_from() {
    let key = be(1);
    let facade = &Facade::new();
    let transaction = facade.db.begin();
    let handle = LigninNodeStore::immediate(&transaction, LigninSubtree::alone(P2));
    (handle.enter(TreeType::MmrTree)).expect("the subtree is entered");
    drop(handle);
    assert_eq!(merk_of_p2(&transaction, None, &[1], &[]), Some(key.clone()));
    facade.commit(&transaction);

    let first = facade.db.begin();
    let handle = LigninNodeStore::immediate(&first, LigninSubtree::alone(P2));
    let old = handle.get_node(&key, NO_VALUE_DEFINED_COST, version());
    let old = old.value.expect("a read").expect("the node");

    // Two commits beside the handle: the Merk is emptied, and a flat
    // record is put under the key.
    let other = facade.db.begin();
    assert_eq!(merk_of_p2(&other, Some(key.clone()), &[], &[1]), None);
    facade.commit(&other);
    let other = facade.db.begin();
    let flat = LigninNodeStore::immediate(&other, LigninSubtree::alone(P2));
    (FlatStore::put(&flat, &key, &[0x5a], None).unwrap()).expect("a put");
    drop(flat);
    facade.commit(&other);

    let no_node = handle.get_node(&key, NO_VALUE_DEFINED_COST, version());
    no_node.value.expect_err("the record is no node");
    let written = handle.write_node(&old, NodeChange::HashOnly).value;
    drop(handle);
    let published = written.is_ok()
        && matches!(
            facade.db.commit(&first, &mut NoStaleHashes),
            Ok(Committed::Published(_))
        );
    assert!(!published, "the keep of a value whose cell is gone");
    let reader = facade.db.begin();
    let store = LigninNodeStore::read_only(&reader, LigninSubtree::alone(P2));
    let stored = FlatStore::get(&store, &key).unwrap().expect("a read");
    assert_eq!(stored, Some(vec![0x5a]));
}

/// A node with no child.
fn leaf(key: &[u8], value: &[u8]) -> TreeNode {
    let feature = TreeFeatureType::BasicMerkNode;
    TreeNode::new(key.to_vec(), value.to_vec(), None, feature).unwrap()
}

/// The value of the node that `handle` reads under `key`.
fn value_under(handle: &LigninNodeStore<'_>, key: &[u8]) -> Vec<u8> {
    let node = handle.get_node(key, NO_VALUE_DEFINED_COST, version());
    let node = node.value.expect("a read").expect("a node");
    node.value_as_slice().to_vec()
}

/// A node that a handle read from a flat record was read from no cell,
/// whatever the handle reads under its key afterwards. Here the record is
/// removed and a Merk's node with another value is committed under the
/// key, and the handle reads that node. A write of the node from the flat
/// record, with a change that keeps its value, states the value it has:
/// the handle then reads that node whole, and not its hashes over the
/// value of the cell. It is so through the handle and through a change
/// set of it, and also where the handle read a second flat record under
/// the key in between.
#[test]
fn a_node_from_a_flat_record_does_not_keep_the_value_of_a_cell_read_later() {
    fn run(through_change_set: bool, a_second_record: bool) {
        let key = be(1);
        let from_flat = leaf(&key, b"the value of the flat record");
        let facade = &Facade::new();
        let subtree = || LigninSubtree::alone(P2);
        let transaction = facade.db.begin();
        let handle = LigninNodeStore::immediate(&transaction, subtree());
        (handle.enter(TreeType::MmrTree)).expect("the subtree is entered");
        (FlatStore::put(&handle, &key, &from_flat.encode(), None).unwrap()).expect("a put");
        drop(handle);
        facade.commit(&transaction);

        let first = facade.db.begin();
        let handle = LigninNodeStore::immediate(&first, subtree());
        let saved = handle.get_node(&key, NO_VALUE_DEFINED_COST, version());
        let saved = saved
            .value
            .expect("a read")
            .expect("the node of the record");
        assert_eq!(saved.encode(), from_flat.encode());

        if a_second_record {
            let second = leaf(&key, b"the value of a second flat record");
            let other = facade.db.begin();
            let flat = LigninNodeStore::immediate(&other, subtree());
            (FlatStore::put(&flat, &key, &second.encode(), None).unwrap()).expect("a put");
            drop(flat);
            facade.commit(&other);
            assert_eq!(value_under(&handle, &key), second.value_as_slice());
        }
        let other = facade.db.begin();
        let flat = LigninNodeStore::immediate(&other, subtree());
        (FlatStore::delete(&flat, &key, None).unwrap()).expect("the record is removed");
        drop(flat);
        facade.commit(&other);
        let other = facade.db.begin();
        assert_eq!(merk_of_p2(&other, None, &[1], &[]), Some(key.clone()));
        facade.commit(&other);

        let of_the_cell = value_under(&handle, &key);
        assert_ne!(of_the_cell, saved.value_as_slice());
        match through_change_set {
            false => {
                let written = handle.write_node(&saved, NodeChange::HashOnly);
                written.value.expect("the node is written with its value");
            }
            true => {
                let mut change_set = handle.change_set();
                let prepared = change_set.prepare_node(&saved, NodeChange::HashOnly);
                change_set.put_node(&key, prepared, None).unwrap();
                let written = handle.commit(change_set);
                written.value.expect("the node is written with its value");
            }
        }
        let back = handle.get_node(&key, NO_VALUE_DEFINED_COST, version());
        let back = back.value.expect("a read").expect("a node");
        let case = (through_change_set, a_second_record);
        assert_eq!(back.encode(), saved.encode(), "{case:?}");
    }
    for through_change_set in [false, true] {
        for a_second_record in [false, true] {
            run(through_change_set, a_second_record);
        }
    }
}

/// A node that a handle read from a cell is written as it would be with
/// no read of a flat record: here, where the handle has read nothing
/// else under the key, it is kept by that cell, also though the flat
/// record that the handle read before had the same value. The handle
/// reads such a flat record, the record is removed, a Merk's node with
/// that value is committed under the key and read by the handle, and
/// then another transaction commits another value to the node. A write of the node the handle read from the cell,
/// with a change that keeps its value, is not published over that
/// commit.
#[test]
fn a_node_from_a_cell_is_kept_by_its_cell_after_a_flat_record_of_the_same_value() {
    let key = be(1);
    let value = flat_value(&key);
    let facade = &Facade::new();
    let subtree = || LigninSubtree::alone(P2);
    let transaction = facade.db.begin();
    let handle = LigninNodeStore::immediate(&transaction, subtree());
    (handle.enter(TreeType::MmrTree)).expect("the subtree is entered");
    let record = leaf(&key, &value).encode();
    (FlatStore::put(&handle, &key, &record, None).unwrap()).expect("a put");
    drop(handle);
    facade.commit(&transaction);

    let first = facade.db.begin();
    let handle = LigninNodeStore::immediate(&first, subtree());
    assert_eq!(value_under(&handle, &key), value);

    let other = facade.db.begin();
    let flat = LigninNodeStore::immediate(&other, subtree());
    (FlatStore::delete(&flat, &key, None).unwrap()).expect("the record is removed");
    drop(flat);
    facade.commit(&other);
    let other = facade.db.begin();
    assert_eq!(merk_of_p2(&other, None, &[1], &[]), Some(key.clone()));
    facade.commit(&other);

    let of_the_cell = handle.get_node(&key, NO_VALUE_DEFINED_COST, version());
    let of_the_cell = of_the_cell.value.expect("a read").expect("the node");
    assert_eq!(of_the_cell.value_as_slice(), value);
    assert!(!of_the_cell.from_flat_record());

    let other = facade.db.begin();
    let batch = [basic(1, b"the value of another commit")];
    assert_eq!(
        merk_batch_of_p2(&other, Some(key.clone()), &batch),
        Some(key.clone())
    );
    facade.commit(&other);

    let written = handle.write_node(&of_the_cell, NodeChange::HashOnly).value;
    drop(handle);
    let published = written.is_ok()
        && matches!(
            facade.db.commit(&first, &mut NoStaleHashes),
            Ok(Committed::Published(_))
        );
    assert!(
        !published,
        "the keep of a value that another commit replaced"
    );
    let reader = facade.db.begin();
    let store = LigninNodeStore::read_only(&reader, subtree());
    assert_eq!(value_under(&store, &key), b"the value of another commit");
}

/// The same for a handle that remembers an unlinked record under the key
/// and then its absence: the node it reads from a flat record afterwards
/// is written back with the value it has, though the handle's first read
/// of the key had no cell and its last read before found none.
#[test]
fn a_node_from_a_flat_record_is_written_back_after_reads_of_other_kinds() {
    fn put<'db, S: NodeStore<'db>>(store: &S, node: &TreeNode) {
        let mut change_set = store.change_set();
        let prepared = change_set.prepare_node(node, NodeChange::ValueWritten);
        change_set.put_node(node.key(), prepared, None).unwrap();
        store.commit(change_set).unwrap().expect("a commit");
    }
    let key = be(1);
    let unlinked = leaf(&key, b"a record no node links");
    let from_flat = leaf(&key, b"the value of the flat record");
    let facade = &Facade::new();
    let subtree = || LigninSubtree::alone(P2);
    let transaction = facade.db.begin();
    let handle = LigninNodeStore::immediate(&transaction, subtree());
    (handle.enter(TreeType::NormalTree)).expect("the subtree is entered");
    put(&handle, &unlinked);
    drop(handle);
    facade.commit(&transaction);

    let first = facade.db.begin();
    let handle = LigninNodeStore::immediate(&first, subtree());
    assert_eq!(value_under(&handle, &key), unlinked.value_as_slice());

    let other = facade.db.begin();
    let removes = LigninNodeStore::immediate(&other, subtree());
    let mut change_set = removes.change_set();
    change_set.delete_node(&key, None);
    (removes.commit(change_set).unwrap()).expect("the record is removed");
    drop(removes);
    facade.commit(&other);
    let none = handle.get_node(&key, NO_VALUE_DEFINED_COST, version());
    assert!(none.value.expect("a read").is_none());

    let other = facade.db.begin();
    let flat = LigninNodeStore::immediate(&other, subtree());
    flat.convert(TreeType::MmrTree)
        .expect("the subtree is stated anew");
    (FlatStore::put(&flat, &key, &from_flat.encode(), None).unwrap()).expect("a put");
    drop(flat);
    facade.commit(&other);

    let read = handle.get_node(&key, NO_VALUE_DEFINED_COST, version());
    let read = read.value.expect("a read").expect("the node of the record");
    assert_eq!(read.encode(), from_flat.encode());
    assert!(read.from_flat_record());
    let written = handle.write_node(&read, NodeChange::HashOnly);
    written.value.expect("the node is written with its value");
    assert_eq!(value_under(&handle, &key), from_flat.value_as_slice());
    drop(handle);
    facade.commit(&first);
    let reader = facade.db.begin();
    let store = LigninNodeStore::read_only(&reader, subtree());
    let stored = FlatStore::get(&store, &key).unwrap().expect("a read");
    let stored = stored.expect("the key holds a record");
    assert_ne!(stored, from_flat.encode(), "the node was written");
}

/// A value that is stated over a flat record is written, and is not
/// compared with the record's bytes, which are no node's value. Here the
/// value of the node a handle read from a flat record is, byte for byte,
/// the record that another commit puts under the key afterwards. The
/// write of the saved node states its value, and the handle reads the
/// node back, through the handle and through a change set of it.
#[test]
fn a_value_stated_over_a_flat_record_is_not_compared_with_its_bytes() {
    for through_change_set in [false, true] {
        let key = be(1);
        let second = leaf(&key, b"the value of the second record");
        let first = leaf(&key, &second.encode());
        let facade = &Facade::new();
        let subtree = || LigninSubtree::alone(P2);
        let transaction = facade.db.begin();
        let handle = LigninNodeStore::immediate(&transaction, subtree());
        (handle.enter(TreeType::MmrTree)).expect("the subtree is entered");
        (FlatStore::put(&handle, &key, &first.encode(), None).unwrap()).expect("a put");
        drop(handle);
        facade.commit(&transaction);

        let reader = facade.db.begin();
        let handle = LigninNodeStore::immediate(&reader, subtree());
        let saved = handle.get_node(&key, NO_VALUE_DEFINED_COST, version());
        let saved = saved
            .value
            .expect("a read")
            .expect("the node of the record");
        let other = facade.db.begin();
        let flat = LigninNodeStore::immediate(&other, subtree());
        (FlatStore::put(&flat, &key, &second.encode(), None).unwrap()).expect("a put");
        drop(flat);
        facade.commit(&other);
        assert_eq!(value_under(&handle, &key), second.value_as_slice());

        match through_change_set {
            false => {
                let written = handle.write_node(&saved, NodeChange::HashOnly);
                written.value.expect("the node is written with its value");
            }
            true => {
                let mut change_set = handle.change_set();
                let prepared = change_set.prepare_node(&saved, NodeChange::HashOnly);
                change_set.put_node(&key, prepared, None).unwrap();
                let written = handle.commit(change_set);
                written.value.expect("the node is written with its value");
            }
        }
        let back = handle.get_node(&key, NO_VALUE_DEFINED_COST, version());
        let back = back.value.expect("a read").expect("a node");
        assert_eq!(back.encode(), saved.encode(), "{through_change_set}");
    }
}
