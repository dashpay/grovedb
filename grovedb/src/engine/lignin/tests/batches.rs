//! The batch pipeline (GROVE-03): the same calls on LigninDB and on
//! RocksDB, with every answer, every `OperationCost` and the root hash
//! after every write compared.
//!
//! - `plain`: `apply_batch` and `apply_operations_without_batching`, with
//!   trees of every kind made and written into by one batch, replaced,
//!   patched and deleted elements, references, batches that are refused,
//!   the options of a batch, and batches inside write transactions.
//! - `partial`: `apply_partial_batch`, whose second segment the caller
//!   chooses once it has seen what the first cost: that cost is compared
//!   too, and so is what the caller was shown as left over.
//! - `flags`: `apply_batch_with_element_flags_update` and its partial
//!   twin with the two callbacks Drive passes, which merge the storage
//!   flags of a replaced element across epochs and split the bytes a
//!   write frees by owner and epoch. Every call of a callback is
//!   compared, with what it was given.
//!
//! Each history runs in a database of its own. The LigninDB database is
//! then closed, opened again and read by the physical checker of the
//! engine, as the histories of `nested` are.

use std::{cell::RefCell, path::Path};

use grovedb_costs::{storage_cost::removal::StorageRemovedBytes, OperationCost};
use grovedb_epoch_based_storage_flags::StorageFlags;
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

use super::{
    nested::{
        assert_same, check, made_by, number, on_both_engines, open, segments, Log, KINDS,
    },
    LigninEngine,
};
use crate::{
    batch::{BatchApplyOptions, QualifiedGroveDbOp},
    engine::Engine,
    reference_path::ReferencePathType,
    Element, ElementFlags, Error, GroveDb, TransactionArg,
};

pub(super) type Op = QualifiedGroveDbOp;

pub(super) fn path(segments: &[&[u8]]) -> Vec<Vec<u8>> {
    segments.iter().map(|segment| segment.to_vec()).collect()
}

pub(super) fn insert(at: &[&[u8]], key: &[u8], element: Element) -> Op {
    Op::insert_or_replace_op(path(at), key.to_vec(), element)
}

pub(super) fn delete(at: &[&[u8]], key: &[u8]) -> Op {
    Op::delete_op(path(at), key.to_vec())
}

pub(super) fn reference(to: &[&[u8]]) -> Element {
    Element::new_reference(ReferencePathType::AbsolutePathReference(path(to)))
}

/// The two callbacks Drive applies a batch with
/// (`grovedb-epoch-based-storage-flags`), which write down every call
/// made of them: what they were given and what they answered.
#[derive(Default)]
struct Drive {
    calls: RefCell<Vec<String>>,
}

impl Drive {
    fn update(
        &self,
        cost: &grovedb_costs::storage_cost::StorageCost,
        old: Option<ElementFlags>,
        new: &mut ElementFlags,
    ) -> Result<bool, Error> {
        let given = format!("update {cost:?} {old:?} {new:?}");
        let updated = StorageFlags::update_element_flags(cost, old, new)
            .map_err(|error| Error::JustInTimeElementFlagsClientError(error.to_string()));
        let mut calls = self.calls.borrow_mut();
        calls.push(format!("{given} -> {updated:?} {new:?}"));
        updated
    }

    fn split(
        &self,
        flags: &mut ElementFlags,
        key: u32,
        value: u32,
    ) -> Result<(StorageRemovedBytes, StorageRemovedBytes), Error> {
        let split = StorageFlags::split_removal_bytes(flags, key, value)
            .map_err(|error| Error::SplitRemovalBytesClientError(error.to_string()));
        let mut calls = self.calls.borrow_mut();
        calls.push(format!("split {flags:?} {key} {value} -> {split:?}"));
        split
    }

    fn into_log(self, name: &str, log: &mut Log) {
        for (at, call) in self.calls.into_inner().into_iter().enumerate() {
            let name = format!("callback {at} of {name}");
            log.answers.push((name, OperationCost::default(), call));
        }
    }
}

/// The calls of a history through one transaction, or through none.
pub(super) struct Calls<'db, 'a, 'l, E: Engine> {
    pub(super) db: &'db GroveDb<E>,
    pub(super) version: &'static GroveVersion,
    pub(super) through: TransactionArg<'db, 'a, E>,
    pub(super) log: &'l mut Log,
}

impl<'db, 'a, 'l, E: Engine> Calls<'db, 'a, 'l, E> {
    pub(super) fn new(
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
    pub(super) fn root(&mut self) {
        let root = self.db.root_hash(self.through, self.version);
        self.log.answer("root".to_owned(), root);
    }

    pub(super) fn batch(&mut self, name: &str, ops: Vec<Op>) {
        self.batch_with(name, ops, None);
    }

    pub(super) fn batch_with(&mut self, name: &str, ops: Vec<Op>, options: Option<BatchApplyOptions>) {
        let applied = (self.db).apply_batch(ops, options, self.through, self.version);
        self.log.answer(format!("batch {name}"), applied);
        self.root();
    }

    pub(super) fn unbatched(&mut self, name: &str, ops: Vec<Op>) {
        let applied =
            (self.db).apply_operations_without_batching(ops, None, self.through, self.version);
        self.log.answer(format!("unbatched {name}"), applied);
        self.root();
    }

    /// `apply_partial_batch`. What the caller is shown between the two
    /// segments, the cost so far and how many paths are left over at each
    /// level, is an answer of its own.
    pub(super) fn partial(
        &mut self,
        name: &str,
        ops: Vec<Op>,
        options: Option<BatchApplyOptions>,
        mut add_on: impl FnMut(&OperationCost) -> Result<Vec<Op>, Error>,
    ) {
        let shown = RefCell::new(Vec::new());
        let applied = (self.db).apply_partial_batch(
            ops,
            options,
            |cost, left_over| {
                let mut levels: Vec<(u32, usize)> = (left_over.iter())
                    .flat_map(|levels| levels.iter())
                    .map(|(level, paths)| (level, paths.len()))
                    .collect();
                levels.sort_unstable();
                shown.borrow_mut().push((cost.clone(), format!("{levels:?}")));
                add_on(cost)
            },
            self.through,
            self.version,
        );
        for (cost, levels) in shown.into_inner() {
            let name = format!("partial batch {name}, shown");
            self.log.answers.push((name, cost, levels));
        }
        self.log.answer(format!("partial batch {name}"), applied);
        self.root();
    }

    /// `apply_batch_with_element_flags_update` with the callbacks of
    /// [`Drive`]. Each call of a callback is an answer of its own.
    fn flags_batch(&mut self, name: &str, ops: Vec<Op>) {
        let drive = Drive::default();
        let applied = (self.db).apply_batch_with_element_flags_update(
            ops,
            None,
            |cost, old, new| drive.update(cost, old, new),
            |flags, key, value| drive.split(flags, key, value),
            self.through,
            self.version,
        );
        drive.into_log(name, self.log);
        self.log.answer(format!("flags batch {name}"), applied);
        self.root();
    }

    /// `apply_partial_batch_with_element_flags_update` with the callbacks
    /// of [`Drive`].
    fn flags_partial(
        &mut self,
        name: &str,
        ops: Vec<Op>,
        mut add_on: impl FnMut(&OperationCost) -> Result<Vec<Op>, Error>,
    ) {
        let drive = Drive::default();
        let applied = (self.db).apply_partial_batch_with_element_flags_update(
            ops,
            None,
            |cost, old, new| drive.update(cost, old, new),
            |flags, key, value| drive.split(flags, key, value),
            |cost, _left_over| {
                drive.calls.borrow_mut().push(format!("shown {cost:?}"));
                add_on(cost)
            },
            self.through,
            self.version,
        );
        drive.into_log(name, self.log);
        self.log.answer(format!("flags partial batch {name}"), applied);
        self.root();
    }

    pub(super) fn get(&mut self, at: &[&[u8]], key: &[u8]) {
        let read = self.db.get(at, key, self.through, self.version);
        let at = path(at);
        let name = segments(&at).iter().chain([&key]).fold(
            String::from("get"),
            |mut name, segment| {
                name.push('/');
                name.push_str(&String::from_utf8_lossy(segment));
                name
            },
        );
        self.log.answer(name, read);
    }

    pub(super) fn is_empty(&mut self, at: &[&[u8]]) {
        let empty = self.db.is_empty_tree(at, self.through, self.version);
        self.log.answer("empty".to_owned(), empty);
    }

    /// A call that may be refused.
    pub(super) fn either(&mut self, call: impl FnOnce(&mut Self)) {
        let before = self.log.refused;
        call(self);
        self.log.on_purpose += self.log.refused - before;
    }

    /// A call that the history makes to be refused.
    pub(super) fn refused(&mut self, call: impl FnOnce(&mut Self)) {
        let before = self.log.refused;
        call(self);
        assert_eq!(self.log.refused, before + 1, "{}", self.log.last());
        self.log.on_purpose += 1;
    }
}

/// The key of the tree of the kind `kind` under the root.
fn top(kind: usize) -> Vec<u8> {
    format!("tree {kind:02}").into_bytes()
}

/// One batch that makes a tree of every kind `version` makes under the
/// root, a tree of the next kind in each, and writes into both: items,
/// numbers, and a reference from the outer tree to an item of the inner.
fn make(made: usize) -> Vec<Op> {
    let mut ops = Vec::new();
    for kind in 0..made {
        let (outer, inner) = (&KINDS[kind], &KINDS[(kind + 1) % made]);
        let top = top(kind);
        let top = top.as_slice();
        ops.push(insert(&[], top, (outer.make)()));
        ops.push(insert(&[top], b"inner", (inner.make)()));
        ops.push(insert(&[top], b"item", Element::new_item(vec![kind as u8; 20])));
        ops.push(insert(&[top], b"number", number(outer.sums, 7 + kind as i64)));
        ops.push(insert(
            &[top, b"inner"],
            b"a",
            number(inner.sums, 100 + kind as i64),
        ));
        ops.push(insert(
            &[top, b"inner"],
            b"b",
            Element::new_item(vec![9; 300]),
        ));
        ops.push(insert(
            &[top],
            b"reference",
            reference(&[top, b"inner", b"b"]),
        ));
    }
    ops
}

/// One batch that changes what [`make`] made, in every tree: an item
/// written again longer, a number replaced, an element of the inner tree
/// deleted, one added, and one patched to ten bytes more.
fn change(made: usize) -> Vec<Op> {
    let mut ops = Vec::new();
    for kind in 0..made {
        let (outer, inner) = (&KINDS[kind], &KINDS[(kind + 1) % made]);
        let top = top(kind);
        let top = top.as_slice();
        ops.push(insert(&[top], b"item", Element::new_item(vec![kind as u8; 90])));
        ops.push(Op::replace_op(
            path(&[top]),
            b"number".to_vec(),
            number(outer.sums, -3),
        ));
        ops.push(delete(&[top, b"inner"], b"a"));
        ops.push(insert(&[top, b"inner"], b"c", number(inner.sums, 12)));
        ops.push(Op::patch_op(
            path(&[top, b"inner"]),
            b"b".to_vec(),
            Element::new_item(vec![8; 310]),
            10,
        ));
    }
    ops
}

fn plain<E: Engine>(dir: &Path, version: &'static GroveVersion) -> Log {
    let db = open::<E>(dir);
    let made = made_by(version);
    let mut log = Log::default();
    let mut calls = Calls::new(&db, version, None, &mut log);

    calls.batch("make", make(made));
    for kind in 0..made {
        let top = top(kind);
        calls.get(&[&top], b"reference");
        calls.get(&[&top], b"inner");
        calls.get(&[], &top);
    }
    calls.batch("change", change(made));
    for kind in 0..made {
        let top = top(kind);
        calls.get(&[&top], b"reference");
        calls.refused(|calls| calls.get(&[&top, b"inner"], b"a"));
        calls.get(&[], &top);
    }
    // The same batch again changes no value.
    calls.batch("change again", change(made));

    let first = top(0);
    let first = first.as_slice();
    // What a batch is refused for: a tree that is not there, an insert of
    // a key that is there where the batch asks that none is, and two
    // operations of one key.
    calls.refused(|calls| {
        calls.batch(
            "under no tree",
            vec![insert(&[b"nothing"], b"item", Element::new_item(vec![1]))],
        );
    });
    calls.refused(|calls| {
        calls.batch(
            "a tree that is no tree",
            vec![insert(&[first, b"item"], b"item", Element::new_item(vec![1]))],
        );
    });
    calls.refused(|calls| {
        let ops = vec![
            insert(&[first], b"twice", Element::new_item(vec![1])),
            insert(&[first], b"twice", Element::new_item(vec![2])),
        ];
        calls.batch("one key twice", ops);
    });
    calls.refused(|calls| {
        let options = BatchApplyOptions {
            validate_insertion_does_not_override: true,
            ..BatchApplyOptions::default()
        };
        let ops = vec![insert(&[first], b"item", Element::new_item(vec![1]))];
        calls.batch_with("over an element", ops, Some(options));
    });
    calls.refused(|calls| {
        let options = BatchApplyOptions {
            validate_insertion_does_not_override_tree: true,
            ..BatchApplyOptions::default()
        };
        let ops = vec![insert(&[first], b"inner", Element::new_item(vec![1]))];
        calls.batch_with("over a tree", ops, Some(options));
    });
    // A batch that is refused has written nothing.
    calls.get(&[first], b"item");
    calls.refused(|calls| calls.get(&[first], b"twice"));

    // With the check of the operations off, the later of two operations of
    // one key stands.
    let options = BatchApplyOptions {
        disable_operation_consistency_check: true,
        ..BatchApplyOptions::default()
    };
    let ops = vec![
        insert(&[first], b"twice", Element::new_item(vec![1])),
        insert(&[first], b"twice", Element::new_item(vec![2; 40])),
    ];
    calls.batch_with("one key twice, unchecked", ops, Some(options));
    calls.get(&[first], b"twice");

    // A batch whose root storage is not free.
    let options = BatchApplyOptions {
        base_root_storage_is_free: false,
        ..BatchApplyOptions::default()
    };
    let ops = vec![
        insert(&[], b"paid root", Element::empty_tree()),
        insert(&[b"paid root"], b"item", Element::new_item(vec![5; 33])),
    ];
    calls.batch_with("paid root", ops, Some(options));

    // The inner tree of the first is emptied by a batch, and one batch
    // then makes it anew under another key and fills it.
    let ops = vec![delete(&[first, b"inner"], b"b"), delete(&[first, b"inner"], b"c")];
    calls.batch("empty the inner tree", ops);
    calls.is_empty(&[first, b"inner"]);
    calls.get(&[first], b"inner");
    let inner = &KINDS[1 % made];
    let ops = vec![
        insert(&[first], b"second", (inner.make)()),
        insert(&[first, b"second"], b"a", number(inner.sums, 1)),
        insert(&[first, b"inner"], b"again", number(inner.sums, 2)),
    ];
    calls.batch("fill both", ops);

    // The operations of a batch, applied one after the other.
    let second = top(1 % made);
    let second = second.as_slice();
    let ops = vec![
        insert(&[second], b"single", Element::new_item(vec![3; 12])),
        insert(&[second], b"new tree", Element::empty_tree()),
        insert(&[second, b"new tree"], b"item", Element::new_item(vec![4; 7])),
        delete(&[second], b"single"),
    ];
    calls.unbatched("four", ops);
    calls.get(&[second, b"new tree"], b"item");

    drop(calls);
    let transaction = db.start_transaction();
    let mut calls = Calls::new(&db, version, Some(&transaction), &mut log);
    // Inside a write transaction: a batch, a savepoint, a batch that is
    // rolled back to it, a batch in its place, and the commit.
    let ops = vec![
        insert(&[], b"in a transaction", Element::empty_tree()),
        insert(&[b"in a transaction"], b"kept", Element::new_item(vec![6; 50])),
        insert(&[first], b"kept", Element::new_item(vec![6; 50])),
    ];
    calls.batch("in a transaction", ops);
    db.set_savepoint(&transaction).expect("a savepoint");
    let ops = vec![
        insert(&[b"in a transaction"], b"undone", Element::empty_tree()),
        insert(&[b"in a transaction", b"undone"], b"item", Element::new_item(vec![7; 9])),
        delete(&[b"in a transaction"], b"kept"),
    ];
    calls.batch("to be undone", ops);
    db.rollback_to_savepoint(&transaction).expect("a rollback");
    calls.root();
    calls.get(&[b"in a transaction"], b"kept");
    let ops = vec![
        insert(&[b"in a transaction"], b"undone", Element::empty_sum_tree()),
        insert(&[b"in a transaction", b"undone"], b"item", Element::new_sum_item(9)),
    ];
    calls.batch("in its place", ops);
    drop(calls);
    let committed = db.commit_transaction(transaction);
    log.answer("commit".to_owned(), committed);

    let mut calls = Calls::new(&db, version, None, &mut log);
    calls.root();
    calls.get(&[b"in a transaction", b"undone"], b"item");
    db.flush().expect("a flush");
    log
}

fn plain_batches_on(version: &'static GroveVersion) {
    on_both_engines!(plain, version);
}

#[test]
fn batches_on_grove_v1() {
    plain_batches_on(&GROVE_VERSIONS[0]);
}

#[test]
fn batches_on_grove_v2() {
    plain_batches_on(&GROVE_VERSIONS[1]);
}

#[test]
fn batches_on_grove_v3() {
    plain_batches_on(&GROVE_VERSIONS[2]);
}

#[test]
fn batches_on_grove_v4() {
    plain_batches_on(&GROVE_VERSIONS[3]);
}

#[test]
fn batches_on_grove_v5() {
    plain_batches_on(&GROVE_VERSIONS[4]);
}

/// The balance of the payer after it was charged for `cost`: a number that
/// differs with every part of a cost.
fn charged(cost: &OperationCost) -> Element {
    let storage = &cost.storage_cost;
    let charge = u64::from(cost.seek_count)
        + cost.storage_loaded_bytes
        + 7 * u64::from(storage.added_bytes)
        + 11 * u64::from(storage.replaced_bytes)
        + 13 * u64::from(storage.removed_bytes.total_removed_bytes())
        + 17 * u64::from(cost.hash_node_calls);
    Element::new_sum_item(1_000_000_000 - charge as i64)
}

fn partial<E: Engine>(dir: &Path, version: &'static GroveVersion) -> Log {
    let db = open::<E>(dir);
    let mut log = Log::default();
    let mut calls = Calls::new(&db, version, None, &mut log);
    let documents: &[&[u8]] = &[b"documents", b"contract", b"type"];
    let ops = vec![
        insert(&[], b"documents", Element::empty_tree()),
        insert(&[], b"balances", Element::empty_sum_tree()),
        insert(&documents[..1], b"contract", Element::empty_tree()),
        insert(&documents[..2], b"type", Element::empty_tree()),
        insert(&[b"balances"], b"payer", Element::new_sum_item(1_000_000_000)),
    ];
    calls.batch("trees", ops);
    let document = |n: u8, len: usize| {
        let key = [b'd', n];
        insert(documents, &key, Element::new_item(vec![n; len]))
    };
    let pay = |cost: &OperationCost| Ok(vec![insert(&[b"balances"], b"payer", charged(cost))]);

    // With nothing added, a partial batch is the batch.
    calls.partial("nothing added", vec![document(1, 80)], None, |_| Ok(Vec::new()));
    // The payer is charged what the first segment cost.
    calls.partial("charged", vec![document(2, 200), document(3, 31)], None, pay);
    calls.get(&[b"balances"], b"payer");
    // The same at every height the first segment can pause at.
    for (n, height) in [(4u8, 0u8), (5, 1), (6, 2), (7, 3)] {
        let options = BatchApplyOptions {
            batch_pause_height: Some(height),
            ..BatchApplyOptions::default()
        };
        let ops = vec![document(n, 40 + usize::from(n)), delete(documents, &[b'd', n - 3])];
        calls.partial(&format!("paused at {height}"), ops, Some(options), pay);
        calls.get(&[b"balances"], b"payer");
    }
    // What is added can write where the first segment wrote.
    calls.partial("added beside", vec![document(8, 60)], None, |cost| {
        let ops = vec![
            insert(&[b"balances"], b"payer", charged(cost)),
            insert(documents, b"added", Element::new_item(vec![9; 25])),
            insert(&documents[..2], b"second type", Element::empty_tree()),
        ];
        Ok(ops)
    });
    calls.get(documents, b"added");

    // A caller that fails, an addition that the batch refuses, and a batch
    // that is refused before the caller is asked.
    calls.refused(|calls| {
        calls.partial("the caller fails", vec![document(9, 70)], None, |_| {
            Err(Error::InvalidBatchOperation("the payer cannot pay"))
        });
    });
    calls.refused(|calls| {
        calls.partial("added under no tree", vec![document(9, 70)], None, |_| {
            Ok(vec![insert(&[b"nothing"], b"item", Element::new_item(vec![1]))])
        });
    });
    calls.refused(|calls| {
        let ops = vec![insert(&[b"nothing"], b"item", Element::new_item(vec![1]))];
        calls.partial("under no tree", ops, None, pay);
    });
    calls.refused(|calls| calls.get(documents, &[b'd', 9]));
    calls.get(&[b"balances"], b"payer");

    drop(calls);
    let transaction = db.start_transaction();
    let mut calls = Calls::new(&db, version, Some(&transaction), &mut log);
    calls.partial("in a transaction", vec![document(10, 90)], None, pay);
    db.set_savepoint(&transaction).expect("a savepoint");
    calls.partial("to be undone", vec![document(11, 90)], None, pay);
    db.rollback_to_savepoint(&transaction).expect("a rollback");
    calls.root();
    calls.partial("in its place", vec![document(12, 12)], None, pay);
    drop(calls);
    let committed = db.commit_transaction(transaction);
    log.answer("commit".to_owned(), committed);
    Calls::new(&db, version, None, &mut log).root();
    db.flush().expect("a flush");
    log
}

fn partial_batches_on(version: &'static GroveVersion) {
    on_both_engines!(partial, version);
}

#[test]
fn partial_batches_on_grove_v1() {
    partial_batches_on(&GROVE_VERSIONS[0]);
}

#[test]
fn partial_batches_on_grove_v2() {
    partial_batches_on(&GROVE_VERSIONS[1]);
}

#[test]
fn partial_batches_on_grove_v3() {
    partial_batches_on(&GROVE_VERSIONS[2]);
}

#[test]
fn partial_batches_on_grove_v4() {
    partial_batches_on(&GROVE_VERSIONS[3]);
}

#[test]
fn partial_batches_on_grove_v5() {
    partial_batches_on(&GROVE_VERSIONS[4]);
}

/// The flags of an element that `owner` wrote in `epoch`.
fn flags_of(epoch: u16, owner: u8) -> Option<ElementFlags> {
    Some(StorageFlags::new_single_epoch(epoch, Some([owner; 32])).to_element_flags())
}

fn flagged(len: usize, epoch: u16, owner: u8) -> Element {
    Element::new_item_with_flags(vec![owner; len], flags_of(epoch, owner))
}

fn flags<E: Engine>(dir: &Path, version: &'static GroveVersion) -> Log {
    let db = open::<E>(dir);
    let mut log = Log::default();
    let mut calls = Calls::new(&db, version, None, &mut log);
    let documents: &[&[u8]] = &[b"documents", b"contract"];
    let ops = vec![
        insert(&[], b"documents", Element::empty_tree_with_flags(flags_of(0, 1))),
        insert(&documents[..1], b"contract", Element::empty_tree_with_flags(flags_of(0, 1))),
        insert(&[], b"balances", Element::empty_sum_tree_with_flags(flags_of(0, 1))),
    ];
    calls.flags_batch("trees", ops);

    // Elements of two owners are made in epoch 0, grow in epochs 1 and 2,
    // shrink in epoch 3 and are removed: the flags of each keep the epoch
    // it was made in and the bytes each later epoch added, and the bytes
    // that are freed are named by owner and epoch.
    let both = |len: [usize; 2], epoch: u16| {
        vec![
            insert(documents, b"of 1", flagged(len[0], epoch, 1)),
            insert(documents, b"of 2", flagged(len[1], epoch, 2)),
            insert(
                &[b"balances"],
                b"of 1",
                Element::new_sum_item_with_flags(i64::from(epoch) + 5, flags_of(epoch, 1)),
            ),
        ]
    };
    calls.flags_batch("made in epoch 0", both([60, 300], 0));
    calls.flags_batch("grown in epoch 1", both([90, 310], 1));
    calls.flags_batch("grown in epoch 2", both([200, 900], 2));
    calls.get(documents, b"of 1");
    calls.get(documents, b"of 2");
    // The same bytes again in a later epoch.
    calls.flags_batch("unchanged in epoch 3", both([200, 900], 3));
    calls.flags_batch("shrunk in epoch 3", both([100, 305], 3));
    calls.flags_batch("shrunk below the first in epoch 4", both([10, 20], 4));
    calls.get(documents, b"of 1");
    calls.get(documents, b"of 2");
    calls.flags_batch("grown again in epoch 5", both([150, 150], 5));

    // A reference and a tree with flags, a tree made and filled by one
    // batch, and an element that is patched.
    let ops = vec![
        insert(
            documents,
            b"reference",
            Element::new_reference_with_flags(
                ReferencePathType::AbsolutePathReference(path(&[documents[0], documents[1], b"of 2"])),
                flags_of(5, 2),
            ),
        ),
        insert(documents, b"index", Element::empty_tree_with_flags(flags_of(5, 2))),
        insert(&[documents[0], documents[1], b"index"], b"entry", flagged(44, 5, 2)),
    ];
    calls.flags_batch("a reference and a tree", ops);
    let ops = vec![
        insert(&[documents[0], documents[1], b"index"], b"entry", flagged(144, 6, 2)),
        insert(&[documents[0], documents[1], b"index"], b"second", flagged(20, 6, 2)),
    ];
    calls.flags_batch("the tree grows in epoch 6", ops);
    calls.get(documents, b"index");

    // An update that names another owner hands the element over, and one
    // whose flags the callback cannot read is the callback's to refuse:
    // the batch fails with its error and writes nothing.
    let ops = vec![insert(documents, b"of 2", flagged(400, 7, 3))];
    calls.flags_batch("another owner", ops);
    calls.get(documents, b"of 2");
    calls.refused(|calls| {
        let unread = Element::new_item_with_flags(vec![1; 30], Some(vec![0, 1]));
        calls.flags_batch("flags the callback cannot read", vec![insert(documents, b"of 1", unread)]);
    });
    calls.get(documents, b"of 1");

    // The partial twin: the payer is charged between the segments.
    let ops = vec![insert(documents, b"of 1", flagged(170, 7, 1))];
    calls.flags_partial("grown in epoch 7", ops, |cost| {
        Ok(vec![insert(
            &[b"balances"],
            b"payer",
            charged(cost),
        )])
    });
    let ops = vec![insert(documents, b"of 2", flagged(15, 8, 2))];
    calls.flags_partial("shrunk in epoch 8", ops, |cost| {
        Ok(vec![insert(
            &[b"balances"],
            b"payer",
            charged(cost),
        )])
    });

    // What is removed is freed to its owner by its epochs, key and value.
    let ops = vec![
        delete(documents, b"of 1"),
        delete(documents, b"reference"),
        delete(&[documents[0], documents[1], b"index"], b"entry"),
        delete(&[b"balances"], b"of 1"),
    ];
    calls.flags_batch("removed", ops);
    calls.get(documents, b"of 2");

    drop(calls);
    let transaction = db.start_transaction();
    let mut calls = Calls::new(&db, version, Some(&transaction), &mut log);
    calls.flags_batch("made in a transaction", vec![insert(documents, b"late", flagged(64, 9, 1))]);
    calls.flags_batch("grown in it", vec![insert(documents, b"late", flagged(128, 10, 1))]);
    db.set_savepoint(&transaction).expect("a savepoint");
    calls.flags_batch("to be undone", vec![insert(documents, b"late", flagged(8, 11, 1))]);
    db.rollback_to_savepoint(&transaction).expect("a rollback");
    calls.flags_batch("shrunk in it", vec![insert(documents, b"late", flagged(70, 12, 1))]);
    drop(calls);
    let committed = db.commit_transaction(transaction);
    log.answer("commit".to_owned(), committed);
    let mut calls = Calls::new(&db, version, None, &mut log);
    calls.root();
    calls.flags_batch("removed after it", vec![delete(documents, b"late")]);
    db.flush().expect("a flush");
    log
}

fn flag_callbacks_on(version: &'static GroveVersion) {
    let log = on_both_engines!(flags, version);
    // The callbacks were called, and bytes were freed by owner and epoch.
    let callbacks = |what: &str| {
        let called = |(name, _, answer): &&(String, OperationCost, String)| {
            name.starts_with("callback") && answer.starts_with(what)
        };
        log.answers.iter().filter(called).count()
    };
    assert!(callbacks("update") > 20, "{}", callbacks("update"));
    assert!(callbacks("split") > 5, "{}", callbacks("split"));
    let sectioned = |(_, cost, _): &&(String, OperationCost, String)| {
        let removed = &cost.storage_cost.removed_bytes;
        matches!(removed, StorageRemovedBytes::SectionedStorageRemoval(_))
    };
    assert!(log.answers.iter().filter(sectioned).count() >= 4);
}

#[test]
fn flag_callbacks_on_grove_v1() {
    flag_callbacks_on(&GROVE_VERSIONS[0]);
}

#[test]
fn flag_callbacks_on_grove_v2() {
    flag_callbacks_on(&GROVE_VERSIONS[1]);
}

#[test]
fn flag_callbacks_on_grove_v3() {
    flag_callbacks_on(&GROVE_VERSIONS[2]);
}

#[test]
fn flag_callbacks_on_grove_v4() {
    flag_callbacks_on(&GROVE_VERSIONS[3]);
}

#[test]
fn flag_callbacks_on_grove_v5() {
    flag_callbacks_on(&GROVE_VERSIONS[4]);
}

/// EXPLORATORY (not for the commit as it is): another element written over
/// a tree that holds elements, by a batch and by a direct insert.
fn over_a_tree<E: Engine>(dir: &Path, version: &'static GroveVersion) -> Log {
    use crate::operations::insert::InsertOptions;
    let db = open::<E>(dir);
    let mut log = Log::default();
    let mut calls = Calls::new(&db, version, None, &mut log);
    let filled = |top: &[u8]| {
        vec![
            insert(&[], top, Element::empty_tree()),
            insert(&[top], b"a", Element::new_item(vec![1; 30])),
            insert(&[top], b"b", Element::new_item(vec![2; 30])),
            insert(&[top], b"nested", Element::empty_tree()),
            insert(&[top, b"nested"], b"c", Element::new_item(vec![3; 30])),
        ]
    };
    for top in [b"one".as_slice(), b"two", b"three", b"four"] {
        calls.batch("filled", filled(top));
    }
    // An item over a tree, by a batch.
    calls.batch("item over one", vec![insert(&[], b"one", Element::new_item(vec![9; 5]))]);
    calls.get(&[], b"one");
    calls.either(|calls| calls.get(&[b"one"], b"a"));
    // A tree over the item again, and what is read under it.
    calls.batch("tree over the item", vec![insert(&[], b"one", Element::empty_tree())]);
    calls.is_empty(&[b"one"]);
    calls.either(|calls| calls.get(&[b"one"], b"a"));
    calls.batch("into it", vec![insert(&[b"one"], b"a", Element::new_item(vec![7; 12]))]);
    calls.get(&[b"one"], b"a");
    calls.either(|calls| calls.get(&[b"one"], b"b"));
    calls.is_empty(&[b"one", b"nested"]);

    // An empty tree over a tree, by a batch that also writes into it.
    let ops = vec![
        insert(&[], b"two", Element::empty_sum_tree()),
        insert(&[b"two"], b"x", Element::new_sum_item(4)),
    ];
    calls.batch("a tree over two, written", ops);
    calls.get(&[], b"two");
    calls.either(|calls| calls.get(&[b"two"], b"a"));
    calls.get(&[b"two"], b"x");

    // An empty tree over a tree, alone.
    calls.batch("a tree over three", vec![insert(&[], b"three", Element::empty_tree())]);
    calls.get(&[], b"three");
    calls.is_empty(&[b"three"]);
    calls.either(|calls| calls.get(&[b"three"], b"a"));

    // A direct insert of an item over a tree.
    let options = InsertOptions {
        validate_insertion_does_not_override_tree: false,
        ..InsertOptions::default()
    };
    let under_the_root: &[&[u8]] = &[];
    let inserted = db.insert(
        under_the_root,
        b"four",
        Element::new_item(vec![8; 6]),
        Some(options),
        None,
        version,
    );
    calls.log.answer("insert an item over four".to_owned(), inserted);
    calls.root();
    calls.get(&[], b"four");
    db.flush().expect("a flush");
    log
}

#[test]
fn exploring_another_element_over_a_tree() {
    for version in [&GROVE_VERSIONS[0], &GROVE_VERSIONS[4]] {
        let dir = tempfile::tempdir().expect("a directory");
        let lignin = over_a_tree::<LigninEngine>(dir.path(), version);
        let other = tempfile::tempdir().expect("a directory");
        let rocksdb = over_a_tree::<crate::RocksDbEngine>(other.path(), version);
        for (call, (lignin, rocksdb)) in lignin.answers.iter().zip(&rocksdb.answers).enumerate() {
            let same = if lignin == rocksdb { "same  " } else { "DIFFER" };
            eprintln!("{same} {call} {} | L {:?} {} | R {:?} {}", lignin.0, lignin.1, lignin.2, rocksdb.1, rocksdb.2);
        }
        let checked = std::panic::catch_unwind(|| check(dir.path(), version));
        eprintln!("checker on grove version {}: {}", version.protocol_version, checked.is_ok());
    }
}
