pub(crate) mod model;
#[cfg(not(feature = "testing"))]
pub(crate) mod store;
#[cfg(feature = "testing")]
pub(crate) use super::testing as store;

mod acceptance;
mod converted;
mod drops;
mod enters;
mod errors;
mod handles;
mod hashes;
mod histories;
mod moves;
mod parked;
pub(crate) mod random;
mod readers;
mod reads;

mod rebase;
mod recovered;
mod savepoints;
#[path = "../../scope/tests/engine.rs"]
mod scopes;
mod subtrees;
mod threads;
mod trees;
mod unlinked;

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
};

use lignin_format::CommitVersion;

use self::{
    model::{Committed, OwnedBatch, OwnedMerk, TxnModel, Val, Write},
    store::{version, Db, Outcome},
};
use super::*;
use crate::snapshot::Versions;

/// A store with the model of its committed records.
pub(crate) struct World<'s> {
    pub(crate) db: &'s Db,
    pub(crate) versions: &'s Versions,
    pub(crate) path: WritePath<'s>,
    pub(crate) model: Committed,
}

/// A write transaction with its model.
pub(crate) struct Tx<'s> {
    pub(crate) txn: WriteTxn<'s>,
    pub(crate) model: RefCell<TxnModel>,
}

impl Tx<'_> {
    /// Forgets the writes of the model: the transaction was rolled back.
    pub(crate) fn forget(&self) {
        *self.model.borrow_mut() = TxnModel::default();
    }
}

impl<'s> World<'s> {
    pub(crate) fn new(db: &'s Db, versions: &'s Versions) -> Self {
        let model = Committed {
            records: BTreeMap::new(),
            version: versions.latest().get(),
            ..Committed::default()
        };
        Self {
            db,
            versions,
            path: db.path(versions),
            model,
        }
    }

    pub(crate) fn latest(&self) -> CommitVersion {
        self.versions.latest()
    }

    pub(crate) fn begin(&self) -> Tx<'s> {
        Tx {
            txn: self.path.begin(),
            model: RefCell::default(),
        }
    }

    /// The change set that makes of the Merk `name`, as `tx` reads it, the
    /// Merk that `edit` leaves.
    pub(crate) fn merk(
        &self,
        tx: &Tx<'s>,
        name: u8,
        edit: impl FnOnce(&mut BTreeMap<Vec<u8>, Vec<u8>>),
    ) -> OwnedMerk {
        let unit = Unit::data(model::subtree(name));
        let view = tx.model.borrow().view(&self.model.records, unit);
        let mut items = Committed::items(&view);
        edit(&mut items);
        OwnedMerk::change(model::subtree(name), &view, &items)
    }

    /// The record that a change of a Merk's root key writes beside the
    /// nodes, as GroveDB writes the element that hosts the Merk, or the
    /// base root key (`transactions.md` 14.1, GM1). Two transactions that
    /// each make the first node of one Merk meet in it (GM3).
    pub(crate) fn root_record(merk: &OwnedMerk) -> Option<Write> {
        let unit = Unit::new(Column::Roots, merk.subtree);
        merk.root.as_ref().map(|root| match root {
            Some(root) => put(unit, b"root", root),
            None => delete(unit, b"root"),
        })
    }

    /// Applies `batch` through a scope of `tx` that merges, and to the
    /// model.
    pub(crate) fn apply(&self, tx: &Tx<'s>, batch: &OwnedBatch) -> Result<Applied, TxnError> {
        let scope = tx.txn.scope()?;
        // The model keeps a value where its records say the node has it:
        // the node is read here, as a writer reads it before it keeps its
        // value, and the write names that read.
        let applied = self.apply_in(tx, &scope, &batch.through(&scope));
        scope.merge();
        applied
    }

    /// Applies `batch` as it is through a scope of `tx` that merges, and
    /// to the model. The batch names the reads of the values it keeps
    /// itself ([`OwnedBatch::reading`]): the test made them when it meant
    /// to, and other commits may have been made since.
    pub(crate) fn apply_as_read(
        &self,
        tx: &Tx<'s>,
        batch: &OwnedBatch,
    ) -> Result<Applied, TxnError> {
        let scope = tx.txn.scope()?;
        let applied = self.apply_in(tx, &scope, batch);
        scope.merge();
        applied
    }

    /// Applies `batch` as it is through `scope`, a scope of `tx` that
    /// stays open, and to the model.
    pub(crate) fn apply_in(
        &self,
        tx: &Tx<'s>,
        scope: &Scope<'_, 's>,
        batch: &OwnedBatch,
    ) -> Result<Applied, TxnError> {
        let first_write = self.latest().get();
        let applied = batch.with(|batch| scope.apply(batch))?;
        self.note(tx, batch, first_write);
        Ok(applied)
    }

    /// Tells the model of `tx` the writes of `batch`, which was applied
    /// with the first-write version `first_write`.
    fn note(&self, tx: &Tx<'s>, batch: &OwnedBatch, first_write: u64) {
        let mut model = tx.model.borrow_mut();
        // Which subtree is below which before each write of the batch,
        // and which Merk has a node: its drops and clears come before its
        // moves, and those before its enters and its change sets.
        let mut hosts = model.hosts(&self.model.hosts);
        let mut records = model.records(&self.model.records);
        let writes = model.writes.len();
        model.roots.retain(|(after, ..)| *after <= writes);
        let mut said = model.said();
        model.bare.retain(|at| *at < writes);
        let mut bare = Vec::new();
        for mut write in batch.writes() {
            if let Write::Leave(left) = &mut write {
                // A drop names one subtree: what is below it goes too, and
                // what the transaction had written there is covered.
                let rooted = left.lay(&hosts, &records, said.0.get(&left.subtree).copied());
                if left.cleared && !rooted {
                    bare.push(model.writes.len());
                }
                let written = |subtree: &Subtree| {
                    let units = Column::ALL.map(|column| Unit::new(column, *subtree));
                    (model.writes.iter())
                        .any(|(write, ..)| units.iter().any(|unit| write.indexed(*unit)))
                };
                left.cut = left.below.iter().copied().filter(written).collect();
            }
            if let Write::Move(moved) = &mut write {
                // Which subtrees the move takes where, and which leave
                // with it: those are what it covers from here on.
                moved.stated = moved.pairs.clone();
                moved.lay(&hosts);
                moved.left = moved.gone.clone();
            }
            said.take(&model.roots, model.writes.len(), &write);
            model::hosted(&mut hosts, &write);
            model::apply(&mut records, &write);
            model.writes.push((write, first_write, false));
        }
        model.bare.extend(bare);
        // What the change sets of the batch state of the roots.
        let writes = model.writes.len();
        let stated = batch.merks.iter().filter_map(|merk| {
            let root = merk.root.as_ref()?;
            Some((writes, merk.subtree, root.is_some()))
        });
        model.roots.extend(stated);
    }

    /// Applies one change of one Merk.
    pub(crate) fn edit(
        &self,
        tx: &Tx<'s>,
        name: u8,
        edit: impl FnOnce(&mut BTreeMap<Vec<u8>, Vec<u8>>),
    ) -> Applied {
        let merks = vec![self.merk(tx, name, edit)];
        let records = merks.iter().filter_map(Self::root_record).collect();
        self.apply(
            tx,
            &OwnedBatch {
                merks,
                records,
                ..OwnedBatch::default()
            },
        )
        .expect("the change is applied")
    }

    /// Applies writes of flat records.
    pub(crate) fn write(&self, tx: &Tx<'s>, records: Vec<Write>) {
        let merks = Vec::new();
        self.apply(
            tx,
            &OwnedBatch {
                merks,
                records,
                ..OwnedBatch::default()
            },
        )
        .expect("the records are written");
    }

    /// Commits `tx` in the store and in the model, and tells the models of
    /// `others`, which stay open.
    pub(crate) fn commit(&mut self, tx: Tx<'s>, others: &[&Tx<'s>]) -> Outcome {
        self.commit_of(&tx, others)
    }

    /// [`Self::commit`], for a transaction that its holder keeps: it has
    /// ended, and the model of it holds no write.
    pub(crate) fn commit_of(&mut self, tx: &Tx<'s>, others: &[&Tx<'s>]) -> Outcome {
        let outcome = self.db.commit(self.versions, &tx.txn).expect("a commit");
        let model = tx.model.take();
        let expected = match (model.writes.is_empty(), model.conflicts()) {
            (true, _) => Outcome::Empty,
            (false, true) => Outcome::Conflict,
            (false, false) => {
                let writes: Vec<Write> = (model.writes.iter())
                    .map(|(write, ..)| write.clone())
                    .collect();
                // The subtrees that the directory has an entry of after
                // the commit, each below the one its entry names. One
                // that went with a subtree above it keeps its entry until
                // it is reclaimed. One that the commit dropped by name,
                // or that a move of it took from its prefix or let
                // leave, has none, unless the commit left a subtree
                // under its prefix.
                let mut stored = self.model.hosts.clone();
                for write in &writes {
                    match write {
                        Write::Leave(left) if !left.cleared => drop(stored.remove(&left.subtree)),
                        Write::Move(moved) => {
                            let taken = moved.pairs.iter().map(|(from, _)| from);
                            for unlinked in taken.chain(&moved.gone) {
                                stored.remove(unlinked);
                            }
                        }
                        _ => {}
                    }
                }
                let commit = self.model.commit(&writes);
                stored.extend(
                    self.model
                        .hosts
                        .iter()
                        .map(|(below, above)| (*below, *above)),
                );
                for other in others {
                    // What the other saw of the subtrees at its last look,
                    // which was before this commit.
                    let looked = other.txn.looked_at().map(CommitVersion::get);
                    let saw = looked.map(|at| self.model.hosts_at(at));
                    let mut other = other.model.borrow_mut();
                    other.look(&saw.unwrap_or_default());
                    other.foreign(commit, &writes);
                    other.foreign_below(commit, &writes, &stored);
                    // Its writes are made again over the commit.
                    other.lay(&self.model.hosts, &self.model.records);
                }
                Outcome::Committed(version(commit))
            }
        };
        assert_eq!(outcome, expected, "the outcome of the commit");
        let stats = tx.txn.stats();
        let held = TxnStats {
            rebuilds: 0,
            handles_followed: 0,
            keyed_reads: 0,
            ..stats
        };
        assert_eq!(
            held,
            TxnStats::default(),
            "a transaction that ended holds nothing"
        );
        outcome
    }

    /// A write made with no transaction: a transaction of its own around
    /// one batch (RD7).
    pub(crate) fn write_outside(&mut self, batch: &OwnedBatch, others: &[&Tx<'s>]) -> Outcome {
        let tx = self.begin();
        self.apply(&tx, batch).expect("the batch is applied");
        self.commit(tx, others)
    }

    /// One change of one Merk, made with no transaction.
    pub(crate) fn edit_outside(
        &mut self,
        name: u8,
        edit: impl FnOnce(&mut BTreeMap<Vec<u8>, Vec<u8>>),
        others: &[&Tx<'s>],
    ) -> Outcome {
        let tx = self.begin();
        self.edit(&tx, name, edit);
        self.commit(tx, others)
    }

    /// The units that the committed records or the writes of `tx` name.
    pub(crate) fn units(&self, tx: &Tx<'s>) -> BTreeSet<Unit> {
        let committed = self.model.records.keys().map(|(unit, _)| *unit);
        let model = tx.model.borrow();
        let written = model.writes.iter().map(|(write, ..)| write.unit());
        // A move writes every column under each prefix it covers, and
        // takes what is under each subtree that leaves with it.
        let moved = model.writes.iter().flat_map(|(write, ..)| match write {
            Write::Move(moved) => moved.covered().chain(moved.gone.clone()).collect(),
            _ => Vec::new(),
        });
        let moved = moved.flat_map(|named| Column::ALL.map(|column| Unit::new(column, named)));
        committed.chain(written).chain(moved).collect()
    }

    /// The keys of `unit` that have, or had, a record that `tx` could read.
    pub(crate) fn keys(&self, tx: &Tx<'s>, unit: Unit) -> BTreeSet<Vec<u8>> {
        let mut keys = model::keys_of(&self.model.records, unit);
        let model = tx.model.borrow();
        for (write, ..) in &model.writes {
            match write {
                Write::Put(own, key, _) | Write::Delete(own, key) if *own == unit => {
                    keys.insert(key.clone());
                }
                _ => {}
            }
        }
        if model.moves() {
            // What a move brought under the unit's prefix.
            keys.extend(model.view(&self.model.records, unit).into_keys());
        }
        keys
    }

    /// Checks every read through `tx` against the model: each record by its
    /// key (RD3, RD5), the committed read of each key (RD10), and a cursor
    /// over each unit of flat records, in both directions (RD6).
    pub(crate) fn check(&self, tx: &Tx<'s>) {
        for unit in self.units(tx) {
            self.check_unit(tx, unit);
        }
        let met = tx.model.borrow().met();
        assert_eq!(tx.txn.stats().overlapped, met, "the overlap set");
    }

    /// [`Self::check`], of the records of `unit` alone.
    pub(crate) fn check_unit(&self, tx: &Tx<'s>, unit: Unit) {
        let committed = &self.model.records;
        let model = tx.model.borrow();
        {
            let view = model.view(committed, unit);
            let mut flat = true;
            for key in self.keys(tx, unit) {
                let expected = model.read(committed, unit, &key);
                let was = committed.get(&(unit, key.clone()));
                let is_node = |value: Option<&Val>| matches!(value, Some(Val::Node(_)));
                if is_node(expected) || is_node(was) {
                    flat = false;
                    let shown = |expected: Option<&Val>, view: Option<NodeView<'_>>| {
                        match (expected, view) {
                            (None, None) => {}
                            (Some(Val::Node(node)), Some(view)) => node.check(&key, &view),
                            (expected, view) => panic!(
                                "the node {key:?} of {unit}: {expected:?} is expected, {view:?} is shown"
                            ),
                        }
                    };
                    (tx.txn
                        .node(unit.subtree, &key, |view| shown(expected, view)))
                    .expect("the node is read");
                    (tx.txn
                        .committed_node(unit.subtree, &key, |view| shown(was, view)))
                    .expect("the committed node is read");
                } else {
                    let flat_of = |value: Option<&Val>| match value {
                        Some(Val::Flat(value)) => Some(value.clone()),
                        _ => None,
                    };
                    let mut out = Vec::new();
                    let found = tx
                        .txn
                        .get(unit, &key, &mut out)
                        .expect("the record is read");
                    assert_eq!(found.then_some(out), flat_of(expected), "{key:?} of {unit}");
                    let mut out = Vec::new();
                    let found = (tx.txn.committed_get(unit, &key, &mut out)).expect("a read");
                    assert_eq!(
                        found.then_some(out),
                        flat_of(was),
                        "committed {key:?} of {unit}"
                    );
                }
            }
            if flat {
                let records: Vec<(Vec<u8>, Vec<u8>)> = view
                    .iter()
                    .map(|(key, value)| match value {
                        Val::Flat(value) => (key.clone(), value.clone()),
                        Val::Node(_) => unreachable!("a unit of flat records"),
                    })
                    .collect();
                assert_eq!(scan(&mut tx.txn.cursor(unit), false), records, "{unit}");
                let mut back = scan(&mut tx.txn.cursor(unit), true);
                back.reverse();
                assert_eq!(back, records, "{unit}, backwards");
            } else {
                let keys: Vec<Vec<u8>> = view.keys().cloned().collect();
                let mut cursor = tx.txn.merk_cursor(unit.subtree).expect("a cursor");
                assert_eq!(scan_merk(&mut cursor, &view, false), keys, "{unit}");
                let mut back = scan_merk(&mut cursor, &view, true);
                back.reverse();
                assert_eq!(back, keys, "{unit}, backwards");
            }
        }
    }

    /// Checks the committed state against the model: every record as a
    /// transaction that has written nothing reads it, and the tree pages.
    pub(crate) fn check_committed(&self) {
        self.check_committed_but(|_| false);
    }

    /// [`Self::check_committed`], where the nodes of the model under the
    /// keys that `unlinked` names are unlinked records, which no tree
    /// page holds.
    pub(crate) fn check_committed_but(&self, unlinked: impl Fn(&[u8]) -> bool) {
        let reader = self.begin();
        self.check(&reader);
        let keys = self.db.check_trees(self.versions, self.latest());
        let mut expected: BTreeMap<Subtree, Vec<Vec<u8>>> = BTreeMap::new();
        for ((unit, key), value) in &self.model.records {
            if matches!(value, Val::Node(_)) && !unlinked(key) {
                expected.entry(unit.subtree).or_default().push(key.clone());
            }
        }
        assert_eq!(keys, expected, "the keys of the committed Merks");
    }
}

/// Every record a cursor shows from one end to the other.
pub(crate) fn scan(cursor: &mut Cursor<'_, '_>, back: bool) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut records = Vec::new();
    match back {
        false => cursor.seek_to_first(),
        true => cursor.seek_to_last(),
    }
    .expect("a seek");
    while let Some(key) = cursor.key().map(<[u8]>::to_vec) {
        let mut value = Vec::new();
        assert!(
            cursor.value(&mut value).expect("a value"),
            "a record under {key:?}"
        );
        records.push((key, value));
        match back {
            false => cursor.next(),
            true => cursor.prev(),
        }
        .expect("a step");
    }
    records
}

/// The key of every node a cursor over a Merk shows from one end to the
/// other, each node checked against the record `view` has under its key.
pub(crate) fn scan_merk(
    cursor: &mut MerkCursor<'_, '_>,
    view: &BTreeMap<Vec<u8>, Val>,
    back: bool,
) -> Vec<Vec<u8>> {
    let mut keys = Vec::new();
    match back {
        false => cursor.seek_to_first(),
        true => cursor.seek_to_last(),
    }
    .expect("a seek");
    while let Some(key) = cursor.key().map(<[u8]>::to_vec) {
        let Some(Val::Node(expected)) = view.get(&key) else {
            panic!("the cursor shows {key:?}, which is no node of the view");
        };
        check_under(cursor, &key, expected);
        keys.push(key);
        match back {
            false => cursor.next(),
            true => cursor.prev(),
        }
        .expect("a step");
    }
    keys
}

/// Checks the node under the key `cursor` stands on against `expected`,
/// and the key of each child as it is read with the node.
fn check_under(cursor: &mut MerkCursor<'_, '_>, key: &[u8], expected: &model::NodeRec) {
    use lignin_format::tree_page::Side;

    let mut children = ChildKeys::new();
    // A record names its children in its links, which `check` compares,
    // and a cell has their keys read with it.
    let is_record = cursor
        .node_with_children(&mut children, |shown| {
            let shown = shown.expect("a node under the cursor's key");
            expected.check(key, &shown);
            shown.is_record() || shown.is_unlinked()
        })
        .expect("the node is read");
    for side in [Side::Left, Side::Right] {
        let child = expected.link(side).map(|link| &link.key[..]);
        let read = child.filter(|_| !is_record).unwrap_or_default();
        assert_eq!(children.key(side), read, "a child of {key:?}");
        assert_eq!(children.take(side), read);
        assert!(children.key(side).is_empty(), "the key was taken");
    }
}

/// The bytes of a node's value, wherever the value is.
pub(crate) fn value_of(view: &NodeView<'_>) -> Vec<u8> {
    let mut buf = lignin_tree::value::ValueBuf::new();
    view.value(&mut buf).expect("the value is read").to_vec()
}

pub(crate) fn flat(value: &[u8]) -> Val {
    Val::Flat(value.to_vec())
}

pub(crate) fn put(unit: Unit, key: &[u8], value: &[u8]) -> Write {
    Write::Put(unit, key.to_vec(), flat(value))
}

pub(crate) fn delete(unit: Unit, key: &[u8]) -> Write {
    Write::Delete(unit, key.to_vec())
}
