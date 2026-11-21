//! The model of a database of OPS-08's tests, over the facade: what each
//! version holds, the batches that take it from one version to the next,
//! and the comparison of what a reader reads with it.
//!
//! The records, the Merks and their hashes are those of the write
//! transaction's own model (`txn/tests/model.rs`), so the physical checker
//! recomputes every hash by the rules of that model
//! (`txn::tests::store::Rules`), the value hashes among them: a value
//! whose bytes are not the ones that were written is a finding.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    commit::{Committed, NoStaleHashes},
    db::WriteError,
    txn::{
        tests::{
            model::{
                self, Committed as Model, OwnedBatch, OwnedMerk, OwnedSubtree, TxnModel, Val, Write,
            },
            put, scan, scan_merk,
            store::Rng,
            World,
        },
        Column, Subtree, Unit,
    },
    Db,
};

/// The subtrees these tests enter are named 1 to this.
pub(crate) const SUBTREES: u8 = 6;

/// The state of the model at one version: its records, and the subtrees
/// in use.
#[derive(Debug, Clone)]
pub(crate) struct State {
    pub(crate) model: Model,
    pub(crate) entered: BTreeSet<Subtree>,
}

impl State {
    /// The state of a database that was made and not written: version 1
    /// made its keyspaces.
    pub(crate) fn new() -> Self {
        Self {
            model: Model {
                version: 1,
                ..Model::default()
            },
            entered: BTreeSet::new(),
        }
    }

    /// The version of the state.
    pub(crate) fn version(&self) -> u64 {
        self.model.version
    }

    /// Commits `batch` as the next version.
    pub(crate) fn commit(&mut self, batch: &OwnedBatch) {
        let hosts = self.model.hosts.clone();
        let mut writes = batch.writes();
        for write in &mut writes {
            if let Write::Leave(left) = write {
                left.below = model::below(&hosts, left.subtree);
                if !left.cleared {
                    self.entered.remove(&left.subtree);
                }
                for below in &left.below {
                    self.entered.remove(below);
                }
            }
        }
        for subtree in &batch.subtrees {
            if let OwnedSubtree::Enter(subtree, _) = subtree {
                self.entered.insert(*subtree);
            }
        }
        self.model.commit(&writes);
    }

    /// The change set that makes of the Merk `name` the Merk that `edit`
    /// leaves.
    pub(crate) fn merk(&self, name: u8, edit: impl FnOnce(&mut Items)) -> OwnedMerk {
        let unit = Unit::data(model::subtree(name));
        let view = TxnModel::default().view(&self.model.records, unit);
        let mut items = Model::items(&view);
        edit(&mut items);
        OwnedMerk::change(model::subtree(name), &view, &items)
    }

    /// Whether the subtree `name` is in use.
    pub(crate) fn has(&self, name: u8) -> bool {
        self.entered.contains(&model::subtree(name))
    }
}

/// A key of these tests: `lead` and a number.
pub(crate) fn key(lead: u8, number: u32) -> Vec<u8> {
    let mut key = vec![lead];
    key.extend_from_slice(&number.to_be_bytes());
    key
}

/// `len` bytes that say what they are: the `revision`th writing of the
/// thing `serial`.
pub(crate) fn value(serial: u32, revision: u8, len: usize) -> Vec<u8> {
    let mut value = vec![revision ^ serial as u8; len];
    if len >= 5 {
        value[..4].copy_from_slice(&serial.to_be_bytes());
        value[4] = revision;
    }
    value
}

/// The batch of `merks` with the record of each changed root and the
/// records `more`, which enters `enter`.
pub(crate) fn batch(
    merks: Vec<OwnedMerk>,
    more: Vec<Write>,
    enter: Vec<OwnedSubtree>,
) -> OwnedBatch {
    let mut records: Vec<Write> = merks.iter().filter_map(World::root_record).collect();
    records.extend(more);
    OwnedBatch {
        merks,
        records,
        subtrees: enter,
    }
}

/// The record that every batch of these tests writes, so that none of
/// them is empty: the number of the write, in the meta column of the
/// subtree 1, which is never dropped.
pub(crate) fn height(number: u64) -> Write {
    put(
        model::unit(Column::Meta, 1),
        b"height",
        &number.to_be_bytes(),
    )
}

/// The items of a Merk, by key.
pub(crate) type Items = BTreeMap<Vec<u8>, Vec<u8>>;

/// Changes to the items of a Merk: a key, and its new value or none.
type Changes = Vec<(Vec<u8>, Option<Vec<u8>>)>;

/// The key of the node of the Merk 1 that hosts the Merk 3.
pub(crate) fn host_key() -> Vec<u8> {
    key(b'a', 5)
}

/// The change sets of a batch that changes the Merk 3: its own, as
/// `three` leaves it, and that of the Merk 1 as `one` leaves it, with the
/// node that hosts the Merk 3 written and naming it. Whoever changes a
/// hosted Merk writes the node that hosts it in the same batch, as GroveDB
/// writes the element above a Merk whose root hash has changed.
pub(crate) fn hosted(
    state: &State,
    number: u64,
    one: impl FnOnce(&mut Items),
    three: impl FnOnce(&mut Items),
) -> Vec<OwnedMerk> {
    let mut host = state.merk(1, |items| {
        one(items);
        // The element above a Merk holds something of it, and so changes
        // with it.
        let len = 30 + (number % 50) as usize;
        items.insert(host_key(), value(5, number as u8, len));
    });
    host.hosted.insert(host_key(), vec![model::subtree(3)]);
    vec![host, state.merk(3, three)]
}

/// The first write of a database of these tests: three Merks, one of them
/// hosted by a node of another, with values in their cells and values out
/// of line, and flat records with a value of each kind.
pub(crate) fn first(state: &State) -> OwnedBatch {
    let mut merks = hosted(
        state,
        1,
        |items| {
            for number in 0..40 {
                items.insert(key(b'a', number), value(number, 1, 30));
            }
        },
        |items| {
            for number in 0..10 {
                items.insert(key(b'c', number), value(number, 1, 200));
            }
        },
    );
    merks.push(state.merk(2, |items| {
        for number in 0..12 {
            items.insert(key(b'b', number), value(number, 1, 3000));
        }
    }));
    batch(
        merks,
        vec![
            put(model::unit(Column::Aux, 1), b"note", &value(7, 1, 3000)),
            put(model::unit(Column::Aux, 2), b"short", b"a few bytes"),
            height(1),
        ],
        vec![
            OwnedSubtree::Enter(model::subtree(1), None),
            OwnedSubtree::Enter(model::subtree(2), None),
            OwnedSubtree::Enter(model::subtree(3), Some((model::subtree(1), host_key(), 0))),
        ],
    )
}

/// A few changes to the items of a Merk whose keys begin with `lead`,
/// drawn from `rng`: each a key with its new value, or with none where it
/// goes. The values are of every length, in a cell and out of line.
fn changes(rng: &mut Rng, lead: u8, number: u64) -> Changes {
    let mut changes = Vec::new();
    for _ in 0..1 + rng.below(5) {
        let at = rng.below(48) as u32;
        // The node that hosts another Merk stays.
        let hosts = (lead, at) == (b'a', 5);
        let value = match rng.below(20) {
            0..=5 if !hosts => None,
            0..=13 => Some(value(at, number as u8, rng.below(200))),
            14..=18 => Some(value(at, number as u8, 1200 + rng.below(2400))),
            _ => Some(Vec::new()),
        };
        changes.push((key(lead, at), value));
    }
    changes
}

fn change(items: &mut Items, changes: &[(Vec<u8>, Option<Vec<u8>>)]) {
    for (key, value) in changes {
        match value {
            Some(value) => items.insert(key.clone(), value.clone()),
            None => items.remove(key),
        };
    }
}

/// A commit that any state takes: the first write of a database that has
/// none, and otherwise one record of the subtree 1.
pub(crate) fn next(state: &State) -> OwnedBatch {
    match state.has(1) {
        true => batch(Vec::new(), vec![height(state.version())], Vec::new()),
        false => first(state),
    }
}

/// Writes the batch of number `number` to `db` and takes it into `state`:
/// the first write, or one that is drawn from `rng`.
pub(crate) fn write(db: &Db, state: &mut State, rng: &mut Rng, number: u64) {
    let batch = match number {
        1 => first(state),
        _ => drawn(state, rng, number),
    };
    match commit(db, &batch, false) {
        Ok(Committed::Published(_)) => state.commit(&batch),
        other => panic!("write {number}: {other:?}"),
    }
}

/// Commits `batch` to `db`, and says how that ended. A batch that states
/// every value it writes is written with no transaction, unless `in_txn`
/// asks for one. A batch that keeps the value of a node is written in a
/// transaction of its own in either case, which reads each such node
/// first: a value that is kept names the read it is kept from
/// (`BatchError::NoRead`), and a write with no transaction has made none.
///
/// A transaction that a refused commit left open is rolled back.
pub(crate) fn commit(db: &Db, batch: &OwnedBatch, in_txn: bool) -> Result<Committed, WriteError> {
    let keeps = (batch.merks.iter()).any(|merk| merk.nodes.iter().any(|(.., stated)| !stated));
    if !in_txn && !keeps {
        return batch.with(|batch| db.write(batch, &mut NoStaleHashes));
    }
    let txn = db.begin();
    let scope = txn.scope().expect("a scope");
    (batch.through(&scope))
        .with(|batch| scope.apply(batch))
        .unwrap_or_else(|error| panic!("the batch is refused: {error}"));
    scope.merge();
    let ended = db.commit(&txn, &mut NoStaleHashes);
    if let Err(error) = &ended {
        // A transaction that a refused commit left open holds its writes
        // still, and gives them up.
        let held = txn.stats().writes > 0;
        assert_eq!(error.leaves_transaction_open(), held, "{error}");
        db.rollback(&txn).expect("a rollback writes to no file");
    }
    ended
}

/// A batch of a few changes to the database as `state` has it, drawn from
/// `rng`: Merks are changed, entered and dropped, with values of every
/// length, and flat records are put, deleted and deleted by range. The
/// subtree 1 stays in use, and so does the Merk 3 that a node of it hosts:
/// the Merks that are dropped stand alone.
pub(crate) fn drawn(state: &State, rng: &mut Rng, number: u64) -> OwnedBatch {
    if !state.has(1) {
        return first(state);
    }
    let mut merks = Vec::new();
    let mut records = vec![height(number)];
    let mut subtrees = Vec::new();
    let free: Vec<u8> = (4..=SUBTREES).filter(|name| !state.has(*name)).collect();
    let alone: Vec<u8> = (4..=SUBTREES).filter(|name| state.has(*name)).collect();
    let mut left = None;
    match rng.below(10) {
        0..=1 if !free.is_empty() => {
            let name = free[rng.below(free.len())];
            subtrees.push(OwnedSubtree::Enter(model::subtree(name), None));
            let count = 1 + rng.below(30) as u32;
            let long = rng.chance(40);
            merks.push(state.merk(name, |items| {
                for at in 0..count {
                    let len = match long {
                        true => 1200 + rng.below(2400),
                        false => rng.below(200),
                    };
                    items.insert(key(b'm', at), value(at, number as u8, len));
                }
            }));
        }
        2 if !alone.is_empty() => {
            let name = alone[rng.below(alone.len())];
            subtrees.push(OwnedSubtree::Drop(model::subtree(name)));
            left = Some(name);
        }
        _ => {}
    }
    let entered: Vec<u8> = (1..=SUBTREES)
        .filter(|name| state.has(*name) && Some(*name) != left)
        .collect();
    let lead = |name: u8| match name {
        1 => b'a',
        2 => b'b',
        3 => b'c',
        _ => b'm',
    };
    let mut chosen: BTreeMap<u8, Changes> = BTreeMap::new();
    for _ in 0..1 + rng.below(2) {
        let name = entered[rng.below(entered.len())];
        let more = changes(rng, lead(name), number);
        chosen.entry(name).or_default().extend(more);
    }
    let changed = |merk: &OwnedMerk| {
        !merk.nodes.is_empty() || !merk.removed.is_empty() || merk.root.is_some()
    };
    if let Some(three) = chosen.remove(&3) {
        let one = chosen.remove(&1).unwrap_or_default();
        merks.extend(hosted(
            state,
            number,
            |items| change(items, &one),
            |items| change(items, &three),
        ));
    }
    for (name, changes) in &chosen {
        merks.push(state.merk(*name, |items| change(items, changes)));
    }
    merks.retain(changed);
    records.extend(merks.iter().filter_map(World::root_record));
    for _ in 0..rng.below(4) {
        let name = entered[rng.below(entered.len())];
        let column = [Column::Aux, Column::Meta][rng.below(2)];
        let unit = model::unit(column, name);
        let at = format!("r{:02}", rng.below(24)).into_bytes();
        let taken = records.iter().any(|write| match write {
            Write::Put(own, key, _) | Write::Delete(own, key) => *own == unit && *key == at,
            Write::Range(own, ..) => *own == unit,
            _ => false,
        });
        if taken || (column == Column::Meta && name == 1) {
            continue;
        }
        let serial = rng.below(1000) as u32;
        records.push(match rng.below(10) {
            0..=4 => put(unit, &at, &value(serial, number as u8, rng.below(150))),
            5..=6 => put(
                unit,
                &at,
                &value(serial, number as u8, 1500 + rng.below(2500)),
            ),
            7..=8 => Write::Delete(unit, at),
            _ => Write::Range(unit, at, None),
        });
    }
    OwnedBatch {
        merks,
        records,
        subtrees,
    }
}

/// Compares what a reader of `db` reads with `state`: every node of every
/// Merk with its value, every flat record, and which subtrees are in use.
pub(crate) fn check_reads(db: &Db, state: &State, context: &str) {
    assert_eq!(
        db.version().get(),
        state.version(),
        "{context}: the version"
    );
    let txn = db.begin();
    let mut units: BTreeSet<Unit> = state.model.records.keys().map(|(unit, _)| *unit).collect();
    for subtree in &state.entered {
        units.extend(Column::ALL.map(|column| Unit::new(column, *subtree)));
    }
    for unit in units {
        assert!(
            state.entered.contains(&unit.subtree),
            "{context}: the model has a record of {unit}, whose subtree is not in use"
        );
        let view = TxnModel::default().view(&state.model.records, unit);
        if view.values().any(|value| matches!(value, Val::Node(_))) {
            let keys: Vec<Vec<u8>> = view.keys().cloned().collect();
            let mut cursor = txn.merk_cursor(unit.subtree).expect("a cursor");
            let shown = scan_merk(&mut cursor, &view, false);
            assert_eq!(shown, keys, "{context}: the nodes of {unit}");
        } else {
            let records: Vec<(Vec<u8>, Vec<u8>)> = view
                .iter()
                .map(|(key, value)| match value {
                    Val::Flat(value) => (key.clone(), value.clone()),
                    Val::Node(_) => unreachable!("a unit of flat records"),
                })
                .collect();
            let shown = scan(&mut txn.cursor(unit), false);
            assert_eq!(shown, records, "{context}: the records of {unit}");
        }
    }
    let snapshot = db.snapshot();
    let operation = snapshot.operation();
    for name in 1..=SUBTREES {
        let opened = operation.open(model::subtree(name)).expect("a lookup");
        assert_eq!(
            opened.is_some(),
            state.has(name),
            "{context}: subtree {name}"
        );
    }
}
