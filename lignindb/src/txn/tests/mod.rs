mod model;
mod store;

mod acceptance;
mod handles;
mod random;
mod reads;
mod rebase;

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
struct World<'s> {
    db: &'s Db,
    versions: &'s Versions,
    path: WritePath<'s>,
    model: Committed,
}

/// A write transaction with its model.
struct Tx<'s> {
    txn: WriteTxn<'s>,
    model: RefCell<TxnModel>,
}

impl Tx<'_> {
    /// Forgets the writes of the model: the transaction was rolled back.
    fn forget(&self) {
        *self.model.borrow_mut() = TxnModel::default();
    }
}

impl<'s> World<'s> {
    fn new(db: &'s Db, versions: &'s Versions) -> Self {
        let model = Committed {
            records: BTreeMap::new(),
            version: versions.latest().get(),
        };
        Self {
            db,
            versions,
            path: db.path(versions),
            model,
        }
    }

    fn latest(&self) -> CommitVersion {
        self.versions.latest()
    }

    fn begin(&self) -> Tx<'s> {
        Tx {
            txn: self.path.begin(),
            model: RefCell::default(),
        }
    }

    /// The change set that makes of the Merk `name`, as `tx` reads it, the
    /// Merk that `edit` leaves.
    fn merk(
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
    fn root_record(merk: &OwnedMerk) -> Option<Write> {
        let unit = Unit::new(Column::Roots, merk.subtree);
        merk.root.as_ref().map(|root| match root {
            Some(root) => put(unit, b"root", root),
            None => delete(unit, b"root"),
        })
    }

    /// Applies `batch` through a scope of `tx` that merges, and to the
    /// model.
    fn apply(&self, tx: &Tx<'s>, batch: &OwnedBatch) -> Result<Applied, TxnError> {
        let first_write = self.latest().get();
        let mut scope = tx.txn.scope()?;
        let applied = batch.with(|batch| scope.apply(batch));
        scope.merge();
        let applied = applied?;
        let writes = batch.writes().into_iter();
        (tx.model.borrow_mut().writes).extend(writes.map(|write| (write, first_write, false)));
        Ok(applied)
    }

    /// Applies one change of one Merk.
    fn edit(
        &self,
        tx: &Tx<'s>,
        name: u8,
        edit: impl FnOnce(&mut BTreeMap<Vec<u8>, Vec<u8>>),
    ) -> Applied {
        let merks = vec![self.merk(tx, name, edit)];
        let records = merks.iter().filter_map(Self::root_record).collect();
        self.apply(tx, &OwnedBatch { merks, records })
            .expect("the change is applied")
    }

    /// Applies writes of flat records.
    fn write(&self, tx: &Tx<'s>, records: Vec<Write>) {
        let merks = Vec::new();
        self.apply(tx, &OwnedBatch { merks, records })
            .expect("the records are written");
    }

    /// Commits `tx` in the store and in the model, and tells the models of
    /// `others`, which stay open.
    fn commit(&mut self, tx: Tx<'s>, others: &[&Tx<'s>]) -> Outcome {
        let outcome = self.db.commit(self.versions, &tx.txn).expect("a commit");
        let model = tx.model.borrow();
        let expected = match (model.writes.is_empty(), model.conflicts()) {
            (true, _) => Outcome::Empty,
            (false, true) => Outcome::Conflict,
            (false, false) => {
                let writes: Vec<Write> = (model.writes.iter())
                    .map(|(write, ..)| write.clone())
                    .collect();
                let commit = self.model.commit(&writes);
                for other in others {
                    other.model.borrow_mut().foreign(commit, &writes);
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
    fn write_outside(&mut self, batch: &OwnedBatch, others: &[&Tx<'s>]) -> Outcome {
        let tx = self.begin();
        self.apply(&tx, batch).expect("the batch is applied");
        self.commit(tx, others)
    }

    /// One change of one Merk, made with no transaction.
    fn edit_outside(
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
    fn units(&self, tx: &Tx<'s>) -> BTreeSet<Unit> {
        let committed = self.model.records.keys().map(|(unit, _)| *unit);
        let model = tx.model.borrow();
        let written = model.writes.iter().map(|(write, ..)| write.unit());
        committed.chain(written).collect()
    }

    /// The keys of `unit` that have, or had, a record that `tx` could read.
    fn keys(&self, tx: &Tx<'s>, unit: Unit) -> BTreeSet<Vec<u8>> {
        let mut keys = model::keys_of(&self.model.records, unit);
        for (write, ..) in &tx.model.borrow().writes {
            match write {
                Write::Put(own, key, _) | Write::Delete(own, key) if *own == unit => {
                    keys.insert(key.clone());
                }
                _ => {}
            }
        }
        keys
    }

    /// Checks every read through `tx` against the model: each record by its
    /// key (RD3, RD5), the committed read of each key (RD10), and a cursor
    /// over each unit of flat records, in both directions (RD6).
    fn check(&self, tx: &Tx<'s>) {
        let committed = &self.model.records;
        let model = tx.model.borrow();
        for unit in self.units(tx) {
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
            }
        }
        assert_eq!(tx.txn.stats().overlapped, model.met(), "the overlap set");
    }

    /// Checks the committed state against the model: every record as a
    /// transaction that has written nothing reads it, and the tree pages.
    fn check_committed(&self) {
        let reader = self.begin();
        self.check(&reader);
        let keys = self.db.check_trees(self.latest());
        let mut expected: BTreeMap<Subtree, Vec<Vec<u8>>> = BTreeMap::new();
        for ((unit, key), value) in &self.model.records {
            if matches!(value, Val::Node(_)) {
                expected.entry(unit.subtree).or_default().push(key.clone());
            }
        }
        assert_eq!(keys, expected, "the keys of the committed Merks");
    }
}

/// Every record a cursor shows from one end to the other.
fn scan(cursor: &mut Cursor<'_, '_>, back: bool) -> Vec<(Vec<u8>, Vec<u8>)> {
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

fn flat(value: &[u8]) -> Val {
    Val::Flat(value.to_vec())
}

fn put(unit: Unit, key: &[u8], value: &[u8]) -> Write {
    Write::Put(unit, key.to_vec(), flat(value))
}

fn delete(unit: Unit, key: &[u8]) -> Write {
    Write::Delete(unit, key.to_vec())
}
