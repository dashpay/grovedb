//! What the writers of the stress test write, piece by piece, and how a
//! reader compares what it reads with the history.

use std::collections::{BTreeMap, VecDeque};

use lignin_format::tree_page::Side;
use lignin_store::load::LoadBuffers;
use lignin_tree::{
    cursor::Cursor,
    value::{read_value, ValueBuf},
};

use super::history::State;
use crate::{
    snapshot::Operation,
    txn::{
        tests::{
            model::{self, Committed, Left, OwnedBatch, OwnedMerk, OwnedSubtree, Val, Write},
            store::{mix, Rng},
            World,
        },
        Column, Subtree, Unit,
    },
    Db,
};

/// The subtree both writers write records of: the keys their commits meet
/// in (`transactions.md` CF2).
pub(in crate::db::tests) fn shared() -> Subtree {
    named(1, 0, 0)
}

fn named(kind: u8, writer: u8, number: u64) -> Subtree {
    let mut prefix = [kind; 32];
    prefix[1] = writer;
    prefix[2..10].copy_from_slice(&number.to_be_bytes());
    Subtree::new(prefix)
}

/// The key that both writers write under `slot` of the shared subtree.
pub(in crate::db::tests) fn contested_key(slot: u8) -> Vec<u8> {
    vec![b'c', slot]
}

/// `len` bytes that say what they are: the `revision`th writing of the
/// thing `serial`. A value that is torn, or another one's, does not read
/// as the model has it.
fn told(serial: u64, revision: u64, len: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(len + 8);
    bytes.extend(serial.to_be_bytes());
    bytes.extend(revision.to_be_bytes());
    let mut state = mix(serial, &revision.to_be_bytes());
    while bytes.len() < len {
        state = mix(state, &[1]);
        bytes.extend(state.to_le_bytes());
    }
    bytes.truncate(len);
    bytes
}

fn document_key(serial: u64) -> Vec<u8> {
    let mut key = vec![b'd'];
    key.extend(serial.to_be_bytes());
    key
}

/// The sizes of the values: some stay in their cell, most go to the value
/// log, where the threshold is 256 bytes.
const SIZES: [usize; 6] = [24, 120, 300, 420, 900, 2_600];

/// The documents a stable Merk is held to.
const DOCUMENTS: usize = 90;
const NOTES: u64 = 5;
const OUTER: u64 = 24;
const INNER: u64 = 12;
/// The key of the node of a generation's outer Merk that hosts the inner.
fn host_key() -> Vec<u8> {
    document_key(1)
}

/// What a writer notes of a piece once the commit that holds it is
/// published.
#[derive(Clone)]
enum Then {
    Nothing,
    /// The nodes that one of its stable Merks has now.
    Merk(usize, BTreeMap<Vec<u8>, Val>),
    Entered(u64),
    Dropped(u64),
    /// Members that joined its crowd, and how many of the oldest left it.
    Crowded(Vec<u64>),
    Thinned(usize),
}

/// One part of a commit: the batch that one scope applies, what it writes
/// as the model has it, and what its writer notes when it is committed.
pub(in crate::db::tests) struct Piece {
    pub(in crate::db::tests) batch: OwnedBatch,
    pub(in crate::db::tests) writes: Vec<Write>,
    then: Then,
}

impl Piece {
    fn new(batch: OwnedBatch, then: Then) -> Self {
        let writes = batch.writes();
        Self {
            batch,
            writes,
            then,
        }
    }
}

/// Commits `batch` through a transaction of its own, in which a scope
/// first reads every node whose value the batch keeps, as a writer does
/// before it changes a node: a kept value names the read it is kept from
/// (CF2a), and a write with no transaction has no read to name.
pub(in crate::db::tests) fn write(
    db: &Db,
    batch: &OwnedBatch,
) -> Result<crate::commit::Committed, crate::db::WriteError> {
    let txn = db.begin();
    let scope = txn.scope().expect("a scope");
    let read = batch.through(&scope);
    let applied = read.with(|batch| scope.apply(batch));
    applied.unwrap_or_else(|error| panic!("a batch of a transaction of its own: {error}"));
    scope.merge();
    db.commit(&txn, &mut crate::commit::NoStaleHashes)
}

/// The batch that makes every write of `pieces` in one operation: the
/// drops first, as a batch states them.
pub(in crate::db::tests) fn joined(pieces: &[Piece]) -> OwnedBatch {
    let mut batch = OwnedBatch::default();
    let subtrees = || pieces.iter().flat_map(|piece| &piece.batch.subtrees);
    let leaves = |subtree: &&OwnedSubtree| !matches!(subtree, OwnedSubtree::Enter(..));
    batch.subtrees.extend(subtrees().filter(leaves).cloned());
    (batch.subtrees).extend(subtrees().filter(|subtree| !leaves(subtree)).cloned());
    for piece in pieces {
        batch.merks.extend(piece.batch.merks.iter().cloned());
        batch.records.extend(piece.batch.records.iter().cloned());
    }
    batch
}

/// One of the two writers: the Merks that are its own, which nobody else
/// writes, and the subtrees it enters and drops again, generation after
/// generation.
#[derive(Clone)]
pub(in crate::db::tests) struct Writer {
    id: u8,
    rng: Rng,
    /// Its stable Merks, each with the nodes its last commit left.
    merks: Vec<(Subtree, BTreeMap<Vec<u8>, Val>)>,
    serial: u64,
    revision: u64,
    /// The generations that are entered, the oldest first.
    generations: VecDeque<u64>,
    generation: u64,
    /// The members of its crowd that are entered, the oldest first.
    crowd: VecDeque<u64>,
    member: u64,
}

impl Writer {
    pub(in crate::db::tests) const MERKS: usize = 3;

    pub(in crate::db::tests) fn new(id: u8, seed: u64) -> Self {
        Self {
            id,
            rng: Rng::new(mix(seed, &[id])),
            merks: (0..Self::MERKS as u64)
                .map(|number| (named(2, id, number), BTreeMap::new()))
                .collect(),
            serial: 0,
            revision: 0,
            generations: VecDeque::new(),
            generation: 0,
            crowd: VecDeque::new(),
            member: 0,
        }
    }

    pub(in crate::db::tests) fn chance(&mut self, percent: usize) -> bool {
        self.rng.chance(percent)
    }

    pub(in crate::db::tests) fn below(&mut self, bound: usize) -> usize {
        self.rng.below(bound)
    }

    /// The subtrees that are entered once, before anything else is
    /// written: the stable Merks of this writer.
    pub(in crate::db::tests) fn enters(&self) -> Vec<OwnedSubtree> {
        (self.merks.iter())
            .map(|(subtree, _)| OwnedSubtree::Enter(*subtree, None))
            .collect()
    }

    pub(in crate::db::tests) fn generations(&self) -> usize {
        self.generations.len()
    }

    fn value(&mut self, serial: u64) -> Vec<u8> {
        self.revision += 1;
        let len = SIZES[self.rng.below(SIZES.len())];
        told(serial, self.revision, len)
    }

    /// A change of the stable Merk `merk`: new documents, some written
    /// again, some deleted, the record of its root key if the root moves,
    /// and a note beside them, whose value is out of line.
    pub(in crate::db::tests) fn documents(&mut self, merk: usize) -> Piece {
        let (subtree, view) = self.merks[merk].clone();
        let mut items = Committed::items(&view);
        let keys: Vec<Vec<u8>> = items.keys().cloned().collect();
        for _ in 0..self.rng.below(3).min(keys.len()) {
            let again = keys[self.rng.below(keys.len())].clone();
            let serial = u64::from_be_bytes(again[1..9].try_into().expect("a serial"));
            let value = self.value(serial);
            items.insert(again, value);
        }
        let over = (items.len() + 4).saturating_sub(DOCUMENTS);
        for _ in 0..over.max(self.rng.below(2)).min(keys.len()) {
            items.remove(&keys[self.rng.below(keys.len())]);
        }
        for _ in 0..1 + self.rng.below(4) {
            self.serial += 1;
            let value = self.value(self.serial);
            items.insert(document_key(self.serial), value);
        }
        let change = OwnedMerk::change(subtree, &view, &items);
        let mut records: Vec<Write> = World::root_record(&change).into_iter().collect();
        let aux = Unit::new(Column::Aux, subtree);
        let note = vec![b'n', self.rng.below(NOTES as usize) as u8];
        records.push(match self.rng.chance(15) {
            true => Write::Delete(aux, note),
            false => {
                self.revision += 1;
                let value = told(
                    u64::from(note[1]),
                    self.revision,
                    2_000 + self.rng.below(900),
                );
                Write::Put(aux, note, Val::Flat(value))
            }
        });
        let after = (model::records(&items).1.into_iter())
            .map(|(key, node)| (key, Val::Node(node)))
            .collect();
        let batch = OwnedBatch {
            merks: vec![change],
            records,
            subtrees: Vec::new(),
        };
        Piece::new(batch, Then::Merk(merk, after))
    }

    /// Every document of the stable Merk `merk` written again, each with a
    /// new value, and nothing else: the Merk keeps its keys, so its root
    /// stays.
    pub(in crate::db::tests) fn rewritten(&mut self, merk: usize) -> Piece {
        let (subtree, view) = self.merks[merk].clone();
        let mut items = Committed::items(&view);
        for (key, value) in &mut items {
            let serial = u64::from_be_bytes(key[1..9].try_into().expect("a serial"));
            *value = self.value(serial);
        }
        let change = OwnedMerk::change(subtree, &view, &items);
        let records = World::root_record(&change).into_iter().collect();
        let after = (model::records(&items).1.into_iter())
            .map(|(key, node)| (key, Val::Node(node)))
            .collect();
        let batch = OwnedBatch {
            merks: vec![change],
            records,
            subtrees: Vec::new(),
        };
        Piece::new(batch, Then::Merk(merk, after))
    }

    fn generation_subtrees(&self, generation: u64) -> (Subtree, Subtree) {
        (named(3, self.id, generation), named(4, self.id, generation))
    }

    /// A new generation: a subtree with a Merk, one of whose nodes hosts
    /// a second subtree with a Merk and records of its own.
    pub(in crate::db::tests) fn generation(&mut self) -> Piece {
        let generation = self.generation;
        self.generation += 1;
        let (outer, inner) = self.generation_subtrees(generation);
        let merk = |writer: &mut Self, subtree: Subtree, nodes: u64| {
            let items: BTreeMap<Vec<u8>, Vec<u8>> = (0..nodes)
                .map(|node| (document_key(node), writer.value(generation * 100 + node)))
                .collect();
            OwnedMerk::change(subtree, &BTreeMap::new(), &items)
        };
        let below = merk(self, inner, INNER);
        let mut above = merk(self, outer, OUTER);
        above.hosted.insert(host_key(), vec![inner]);
        let mut records: Vec<Write> = [&below, &above]
            .into_iter()
            .filter_map(World::root_record)
            .collect();
        let value = Val::Flat(self.value(generation));
        records.push(Write::Put(
            Unit::new(Column::Aux, inner),
            b"aux".to_vec(),
            value,
        ));
        let value = Val::Flat(self.value(generation));
        records.push(Write::Put(
            Unit::new(Column::Meta, outer),
            b"meta".to_vec(),
            value,
        ));
        let batch = OwnedBatch {
            merks: vec![below, above],
            records,
            subtrees: vec![
                OwnedSubtree::Enter(outer, None),
                OwnedSubtree::Enter(inner, Some((outer, host_key(), 0))),
            ],
        };
        Piece::new(batch, Then::Entered(generation))
    }

    /// The drop of the oldest generation that is entered, with the subtree
    /// below it.
    pub(in crate::db::tests) fn drop_oldest(&mut self) -> Option<Piece> {
        let generation = *self.generations.front()?;
        let (outer, inner) = self.generation_subtrees(generation);
        let batch = OwnedBatch {
            subtrees: vec![OwnedSubtree::Drop(outer)],
            ..OwnedBatch::default()
        };
        let mut piece = Piece::new(batch, Then::Dropped(generation));
        piece.writes = vec![Write::Leave(Left {
            subtree: outer,
            cleared: false,
            below: vec![inner],
            cut: Vec::new(),
        })];
        Some(piece)
    }

    /// `count` new members of its crowd, in one batch: subtrees that each
    /// hold a Merk of one node. Each adds an entry and a root record to the
    /// subtree directory, so a crowd is what gives the directory's keyspace
    /// leaves to split.
    pub(in crate::db::tests) fn crowd(&mut self, count: u64) -> Piece {
        let members: Vec<u64> = (self.member..self.member + count).collect();
        self.member += count;
        let mut batch = OwnedBatch::default();
        for member in &members {
            let subtree = named(5, self.id, *member);
            let items = BTreeMap::from([(document_key(*member), told(*member, 0, 40))]);
            let merk = OwnedMerk::change(subtree, &BTreeMap::new(), &items);
            batch.records.extend(World::root_record(&merk));
            batch.merks.push(merk);
            batch.subtrees.push(OwnedSubtree::Enter(subtree, None));
        }
        Piece::new(batch, Then::Crowded(members))
    }

    /// The drop of the `count` oldest members of its crowd, in one batch:
    /// their entries and root records leave the directory, whose leaves
    /// are merged and freed for it.
    pub(in crate::db::tests) fn thin(&mut self, count: usize) -> Option<Piece> {
        let count = count.min(self.crowd.len());
        let leaving = (self.crowd.iter().take(count)).map(|member| named(5, self.id, *member));
        let leaving: Vec<Subtree> = leaving.collect();
        let batch = OwnedBatch {
            subtrees: leaving
                .iter()
                .map(|subtree| OwnedSubtree::Drop(*subtree))
                .collect(),
            ..OwnedBatch::default()
        };
        let mut piece = Piece::new(batch, Then::Thinned(count));
        piece.writes = (leaving.into_iter())
            .map(|subtree| {
                Write::Leave(Left {
                    subtree,
                    cleared: false,
                    below: Vec::new(),
                    cut: Vec::new(),
                })
            })
            .collect();
        (count > 0).then_some(piece)
    }

    /// The removal of the one node of each of the `count` newest members
    /// of its crowd, in one batch. The members stay entered, and the page
    /// that held the node of each keeps no cell.
    pub(in crate::db::tests) fn empty_newest(&mut self, count: usize) -> Piece {
        let mut batch = OwnedBatch::default();
        for member in self.crowd.iter().rev().take(count) {
            let subtree = named(5, self.id, *member);
            let items = BTreeMap::from([(document_key(*member), told(*member, 0, 40))]);
            let view = (model::records(&items).1.into_iter())
                .map(|(key, node)| (key, Val::Node(node)))
                .collect();
            let merk = OwnedMerk::change(subtree, &view, &BTreeMap::new());
            batch.records.extend(World::root_record(&merk));
            batch.merks.push(merk);
        }
        Piece::new(batch, Then::Nothing)
    }

    pub(in crate::db::tests) fn crowded(&self) -> usize {
        self.crowd.len()
    }

    /// A write of the key under `slot` that both writers write.
    pub(in crate::db::tests) fn contested(&mut self, slot: u8) -> Piece {
        let unit = Unit::new(Column::Aux, shared());
        let value = Val::Flat(self.value(u64::from(self.id)));
        let batch = OwnedBatch {
            records: vec![Write::Put(unit, contested_key(slot), value)],
            ..OwnedBatch::default()
        };
        Piece::new(batch, Then::Nothing)
    }

    /// Notes that `pieces` are committed.
    pub(in crate::db::tests) fn committed(&mut self, pieces: Vec<Piece>) {
        for piece in pieces {
            match piece.then {
                Then::Nothing => {}
                Then::Merk(merk, nodes) => self.merks[merk].1 = nodes,
                Then::Entered(generation) => self.generations.push_back(generation),
                Then::Dropped(generation) => {
                    assert_eq!(self.generations.pop_front(), Some(generation));
                }
                Then::Crowded(members) => self.crowd.extend(members),
                Then::Thinned(count) => drop(self.crowd.drain(..count)),
            }
        }
    }
}

/// What a check of one subtree read.
#[derive(Debug, Default, Clone, Copy)]
pub(in crate::db::tests) struct Read {
    pub(in crate::db::tests) nodes: u64,
    pub(in crate::db::tests) records: u64,
    pub(in crate::db::tests) absent: u64,
}

impl std::ops::AddAssign for Read {
    fn add_assign(&mut self, other: Self) {
        self.nodes += other.nodes;
        self.records += other.records;
        self.absent += other.absent;
    }
}

/// Keys with the value the history has under each.
type Valued = Vec<(Vec<u8>, std::sync::Arc<Val>)>;

/// What the history has of one subtree at one version.
pub(in crate::db::tests) struct Expected {
    subtree: Subtree,
    version: u64,
    entered: bool,
    nodes: Valued,
    flat: Vec<(Column, Valued)>,
}

impl Expected {
    /// Takes what `state` has of `subtree` at `version`, so that the lock
    /// on the history is not held over the reads of the database.
    pub(in crate::db::tests) fn of(state: &State, subtree: Subtree, version: u64) -> Self {
        let flat = [Column::Aux, Column::Roots, Column::Meta]
            .map(|column| (column, state.unit(Unit::new(column, subtree), version)));
        Self {
            subtree,
            version,
            entered: state.is_entered(subtree, version),
            nodes: state.unit(Unit::data(subtree), version),
            flat: flat.into(),
        }
    }

    /// Reads the subtree through `operation`, which is of the version this
    /// was taken at, and compares: whether it is entered, every node of
    /// its Merk in key order with its value from wherever that is, its
    /// hashes and its links, and every record of the other columns by a
    /// cursor and by its key.
    pub(in crate::db::tests) fn check(&self, db: &Db, operation: &Operation<'_>) -> Read {
        let (subtree, version) = (self.subtree, self.version);
        assert_eq!(operation.at().get(), version);
        let what = format_args!("{subtree} at version {version}").to_string();
        let opened = operation.open(subtree);
        let opened = opened.unwrap_or_else(|error| panic!("{what}: the directory: {error}"));
        let Some(opened) = opened else {
            assert!(!self.entered, "{what}: it is entered, and is not opened");
            return Read {
                absent: 1,
                ..Read::default()
            };
        };
        assert!(self.entered, "{what}: it is opened, and is not entered");
        let mut read = Read::default();

        let reader = operation.nodes();
        let (mut buffers, mut buf) = (LoadBuffers::new(), ValueBuf::new());
        let mut cursor = Cursor::open(&reader, opened.root());
        let mut node = cursor.seek_to_first(&reader, &mut buffers);
        let mut expected = self.nodes.iter();
        loop {
            let pinned = node.unwrap_or_else(|error| panic!("{what}: a node: {error}"));
            let (Some(pinned), Some((key, record))) = (&pinned, expected.next()) else {
                assert!(
                    pinned.is_none() && expected.next().is_none(),
                    "{what}: the Merk has {} nodes in the model, and the cursor ends elsewhere",
                    self.nodes.len()
                );
                break;
            };
            let Val::Node(record) = &**record else {
                panic!("{what}: a flat record among the nodes");
            };
            let shown = pinned.node();
            assert_eq!(shown.key(), key, "{what}: the keys of the Merk, in order");
            let value = read_value(&db.shared.store.values, pinned, &mut buf);
            let value =
                value.unwrap_or_else(|error| panic!("{what}: the value of {key:?}: {error}"));
            assert!(value == record.value, "{what}: the value of {key:?}");
            assert_eq!(shown.feature(), record.feature, "{what}: {key:?}");
            assert_eq!(shown.value_hash(), &record.value_hash, "{what}: {key:?}");
            assert_eq!(shown.kv_hash(), &record.kv_hash, "{what}: {key:?}");
            for side in [Side::Left, Side::Right] {
                let (link, expected) = (shown.link(side), record.link(side));
                assert_eq!(
                    link.map(|link| (link.hash, link.heights)),
                    expected.map(|link| (&link.hash, link.heights)),
                    "{what}: a link of {key:?}"
                );
            }
            read.nodes += 1;
            node = cursor.next(&reader, &mut buffers);
        }

        let mut value = Vec::new();
        for (column, records) in &self.flat {
            let mut cursor = opened.cursor(*column);
            let moved = cursor.seek_to_first();
            moved.unwrap_or_else(|error| panic!("{what}: a seek in {column}: {error}"));
            for (key, record) in records {
                let Val::Flat(record) = &**record else {
                    panic!("{what}: a node among the records of {column}");
                };
                assert_eq!(cursor.key(), Some(&key[..]), "{what}: the keys of {column}");
                value.clear();
                let found = cursor.value(&mut value);
                let found = found.unwrap_or_else(|error| panic!("{what}: {key:?}: {error}"));
                assert!(
                    found && value == *record,
                    "{what}: the record {key:?} of {column}"
                );
                value.clear();
                let found = opened.get(*column, key, &mut value);
                let found = found.unwrap_or_else(|error| panic!("{what}: {key:?}: {error}"));
                assert!(
                    found && value == *record,
                    "{what}: {key:?} of {column} by its key"
                );
                read.records += 1;
                let moved = cursor.next();
                moved.unwrap_or_else(|error| panic!("{what}: a step in {column}: {error}"));
            }
            assert_eq!(
                cursor.key(),
                None,
                "{what}: a record of {column} the model has not"
            );
        }
        read
    }
}

#[test]
fn a_writers_pieces_say_what_they_write() {
    let mut writer = Writer::new(1, 7);
    assert_eq!(writer.enters().len(), Writer::MERKS);
    assert!(writer.drop_oldest().is_none());
    // A Merk grows to its bound and stays there.
    for _ in 0..200 {
        let piece = writer.documents(0);
        assert_eq!(piece.batch.merks.len(), 1);
        assert!(piece.writes.len() >= 2, "a node and a note at least");
        writer.committed(vec![piece]);
    }
    let nodes = writer.merks[0].1.len();
    assert!(
        (DOCUMENTS / 2..=DOCUMENTS).contains(&nodes),
        "{nodes} nodes"
    );
    // A piece that is not committed leaves the writer's book as it was.
    let _ = writer.documents(0);
    assert_eq!(writer.merks[0].1.len(), nodes);

    let entered = writer.generation();
    let enters = |write: &&Write| matches!(write, Write::Enter(..));
    assert_eq!(entered.writes.iter().filter(enters).count(), 2);
    writer.committed(vec![entered]);
    assert_eq!(writer.generations(), 1);
    let dropped = writer.drop_oldest().expect("a generation is entered");
    let [Write::Leave(left)] = &dropped.writes[..] else {
        panic!("a drop is one write");
    };
    assert_eq!(left.below.len(), 1);
    let both = joined(&[writer.generation(), dropped]);
    assert!(matches!(both.subtrees[0], OwnedSubtree::Drop(_)));
    assert_eq!((both.subtrees.len(), both.merks.len()), (3, 2));
    assert_eq!(writer.contested(1).writes.len(), 1);
    assert!(writer.thin(3).is_none(), "nobody is in the crowd");
    let crowd = writer.crowd(5);
    assert_eq!(
        (crowd.batch.merks.len(), crowd.batch.subtrees.len()),
        (5, 5)
    );
    writer.committed(vec![crowd]);
    let thinned = writer.thin(3).expect("five are in the crowd");
    assert_eq!(thinned.writes.len(), 3);
    writer.committed(vec![thinned]);
    assert_eq!(writer.crowded(), 2);
    assert!(writer.below(4) < 4);
    let _ = writer.chance(50);
    assert_eq!(told(3, 4, 300).len(), 300);
    assert_ne!(told(3, 4, 300), told(3, 5, 300));
    let mut read = Read::default();
    read += Read {
        nodes: 2,
        records: 1,
        absent: 1,
    };
    assert_eq!((read.nodes, read.records, read.absent), (2, 1, 1));
}
