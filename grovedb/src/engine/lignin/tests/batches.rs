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
//! - `over_a_tree`: another element written over a tree that holds
//!   elements (D-133). LigninDB drops the Merk of the tree where RocksDB
//!   leaves its records under the prefix. The writes, their costs, the
//!   root hashes and the proofs are the same. `where_a_tree_was` holds
//!   the reads that are not, which `GROVE-03-F1` is to make RocksDB's
//!   (D-227).
//! - `over_flat_trees`: the same with trees that keep their data flat,
//!   by a partial batch, and for a tree with a tree in it.
//! - `wrong_hashes`: a grove with wrong hashes planted at several depths,
//!   and what `verify_grovedb` reports of it on each engine.
//! - `bidirectional`: batches that write elements which take backward
//!   references and the references to them, on the grove versions whose
//!   batches maintain them: chains, a target written again, a reference
//!   pointed at another target, deletes that cascade, and what a batch is
//!   refused for, the bound on the depth of a reference among it.
//!
//! Each history runs in a database of its own. The LigninDB database is
//! then closed, opened again and read by the physical checker of the
//! engine, as the histories of `nested` are.

use std::{cell::RefCell, path::Path};

use grovedb_costs::{storage_cost::removal::StorageRemovedBytes, OperationCost};
use grovedb_epoch_based_storage_flags::StorageFlags;
use grovedb_merk::element::{
    insert::ElementInsertToStorageExtensions, tree_type::ElementTreeTypeExtensions,
};
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

use super::{
    moves::filled,
    nested::{assert_same, check, made_by, number, on_both_engines, open, segments, Log, KINDS},
    LigninEngine,
};
use crate::{
    batch::{BatchApplyOptions, QualifiedGroveDbOp},
    bidirectional_references::{BidirectionalReference, MAX_BACKWARD_REFERENCES_GROVE_DEPTH},
    engine::Engine,
    query_result_type::QueryResultType,
    reference_path::ReferencePathType,
    Element, ElementFlags, Error, GroveDb, PathQuery, Query, TransactionArg, TreeType,
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

/// The name of a call at a path.
fn named(call: &str, at: &[&[u8]]) -> String {
    at.iter().fold(String::from(call), |mut name, segment| {
        name.push('/');
        name.push_str(&String::from_utf8_lossy(segment));
        name
    })
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

    pub(super) fn batch_with(
        &mut self,
        name: &str,
        ops: Vec<Op>,
        options: Option<BatchApplyOptions>,
    ) {
        let applied = (self.db).apply_batch(ops, options, self.through, self.version);
        self.log.answer(format!("batch {name}"), applied);
        self.root();
    }

    pub(super) fn unbatched(&mut self, name: &str, ops: Vec<Op>) {
        self.unbatched_with(name, ops, None);
    }

    fn unbatched_with(&mut self, name: &str, ops: Vec<Op>, options: Option<BatchApplyOptions>) {
        let applied =
            (self.db).apply_operations_without_batching(ops, options, self.through, self.version);
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
                shown
                    .borrow_mut()
                    .push((cost.clone(), format!("{levels:?}")));
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
        self.log
            .answer(format!("flags partial batch {name}"), applied);
        self.root();
    }

    /// One element inserted with no batch.
    pub(super) fn insert(&mut self, at: &[&[u8]], key: &[u8], element: Element) {
        let inserted = (self.db).insert(at, key, element, None, self.through, self.version);
        self.log.answer("insert".to_owned(), inserted);
        self.root();
    }

    /// Every element of the tree at `at`, as a query reads the range.
    pub(super) fn all(&mut self, at: &[&[u8]]) {
        let mut query = Query::new();
        query.insert_all();
        let query = PathQuery::new_unsized(path(at), query);
        let pairs = QueryResultType::QueryKeyElementPairResultType;
        let read = (self.db).query_raw(&query, true, true, true, pairs, self.through, self.version);
        self.log.answer(named("all", at), read);
    }

    /// The proof of every element of the tree at `at` in the committed
    /// state, and what a verifier makes of it: the root hash it comes to
    /// and the elements it is shown.
    pub(super) fn proven(&mut self, at: &[&[u8]]) {
        let mut query = Query::new();
        query.insert_all();
        let query = PathQuery::new_unsized(path(at), query);
        let proved = self.db.prove_query(&query, None, self.version);
        let proof = proved.value.as_ref().ok().cloned().unwrap_or_default();
        self.log.answer(named("proof", at), proved);
        let verified = GroveDb::verify_query(&proof, &query, self.version);
        let answer = (
            named("verified", at),
            OperationCost::default(),
            super::whole::text(&verified),
        );
        self.log.answers.push(answer);
    }

    /// The paths at which the verification of the grove finds an issue,
    /// references and backward references among what it checks.
    pub(super) fn verify(&mut self) {
        let issues = (self.db).verify_grovedb(self.through, true, true, self.version);
        let issues = issues.map(|issues| {
            let at = |path: &Vec<Vec<u8>>| named("at", &segments(path));
            let mut paths: Vec<String> = issues.keys().map(at).collect();
            paths.sort();
            paths
        });
        let answer = (
            "verify".to_owned(),
            OperationCost::default(),
            format!("{issues:?}"),
        );
        self.log.answers.push(answer);
    }

    pub(super) fn get(&mut self, at: &[&[u8]], key: &[u8]) {
        let read = self.db.get(at, key, self.through, self.version);
        let at = path(at);
        let name =
            segments(&at)
                .iter()
                .chain([&key])
                .fold(String::from("get"), |mut name, segment| {
                    name.push('/');
                    name.push_str(&String::from_utf8_lossy(segment));
                    name
                });
        self.log.answer(name, read);
    }

    pub(super) fn is_empty(&mut self, at: &[&[u8]]) {
        let empty = self.db.is_empty_tree(at, self.through, self.version);
        self.log.answer(named("empty", at), empty);
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
        ops.push(insert(
            &[top],
            b"item",
            Element::new_item(vec![kind as u8; 20]),
        ));
        ops.push(insert(
            &[top],
            b"number",
            number(outer.sums, 7 + kind as i64),
        ));
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
        ops.push(insert(
            &[top],
            b"item",
            Element::new_item(vec![kind as u8; 90]),
        ));
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
            vec![insert(
                &[first, b"item"],
                b"item",
                Element::new_item(vec![1]),
            )],
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
    let ops = vec![
        delete(&[first, b"inner"], b"b"),
        delete(&[first, b"inner"], b"c"),
    ];
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
        insert(
            &[second, b"new tree"],
            b"item",
            Element::new_item(vec![4; 7]),
        ),
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
        insert(
            &[b"in a transaction"],
            b"kept",
            Element::new_item(vec![6; 50]),
        ),
        insert(&[first], b"kept", Element::new_item(vec![6; 50])),
    ];
    calls.batch("in a transaction", ops);
    db.set_savepoint(&transaction).expect("a savepoint");
    let ops = vec![
        insert(&[b"in a transaction"], b"undone", Element::empty_tree()),
        insert(
            &[b"in a transaction", b"undone"],
            b"item",
            Element::new_item(vec![7; 9]),
        ),
        delete(&[b"in a transaction"], b"kept"),
    ];
    calls.batch("to be undone", ops);
    db.rollback_to_savepoint(&transaction).expect("a rollback");
    calls.root();
    calls.get(&[b"in a transaction"], b"kept");
    let ops = vec![
        insert(&[b"in a transaction"], b"undone", Element::empty_sum_tree()),
        insert(
            &[b"in a transaction", b"undone"],
            b"item",
            Element::new_sum_item(9),
        ),
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
        insert(
            &[b"balances"],
            b"payer",
            Element::new_sum_item(1_000_000_000),
        ),
    ];
    calls.batch("trees", ops);
    let document = |n: u8, len: usize| {
        let key = [b'd', n];
        insert(documents, &key, Element::new_item(vec![n; len]))
    };
    let pay = |cost: &OperationCost| Ok(vec![insert(&[b"balances"], b"payer", charged(cost))]);

    // With nothing added, a partial batch is the batch.
    calls.partial("nothing added", vec![document(1, 80)], None, |_| {
        Ok(Vec::new())
    });
    // The payer is charged what the first segment cost.
    calls.partial(
        "charged",
        vec![document(2, 200), document(3, 31)],
        None,
        pay,
    );
    calls.get(&[b"balances"], b"payer");
    // The same at every height the first segment can pause at.
    for (n, height) in [(4u8, 0u8), (5, 1), (6, 2), (7, 3)] {
        let options = BatchApplyOptions {
            batch_pause_height: Some(height),
            ..BatchApplyOptions::default()
        };
        let ops = vec![
            document(n, 40 + usize::from(n)),
            delete(documents, &[b'd', n - 3]),
        ];
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
    // A first segment that stops below the element of a tree whose Merk
    // it writes: the first nodes of a tree, and then nodes that move its
    // root, at every height below which the tree is.
    let second: &[&[u8]] = &[b"documents", b"contract", b"second type"];
    for height in [3u8, 2, 1] {
        let options = BatchApplyOptions {
            batch_pause_height: Some(height),
            ..BatchApplyOptions::default()
        };
        let in_a_row = |n: u8| {
            insert(
                second,
                &[b's', height * 8 + n],
                Element::new_item(vec![n; 30]),
            )
        };
        let ops = (0..3).map(in_a_row).collect();
        calls.partial(
            &format!("three in a row, paused at {height}"),
            ops,
            Some(options),
            pay,
        );
    }
    calls.get(second, &[b's', 25]);
    // Nodes that move the root of a tree which is entered, in a first
    // segment that stops below the tree's element or above it: at every
    // height, for a tree at every depth. Keys in their order move the
    // root where the tree grows to 6, 12, 24 and 48 nodes.
    let grown: [&[&[u8]]; 3] = [
        &[b"grown"],
        &[b"documents", b"grown"],
        &[b"documents", b"contract", b"grown"],
    ];
    let node = |at: &[&[u8]], n: u8| insert(at, &[n], Element::new_item(vec![n; 10]));
    let mut ops = Vec::new();
    for at in grown {
        let (key, above) = at.split_last().expect("a key");
        ops.push(insert(above, key, Element::empty_tree()));
        ops.extend((0..3).map(|n| node(at, n)));
    }
    calls.batch("trees to grow", ops);
    let mut size = 3u8;
    for height in [0u8, 1, 2, 3] {
        let options = BatchApplyOptions {
            batch_pause_height: Some(height),
            ..BatchApplyOptions::default()
        };
        for at in grown {
            let ops = (size..size * 2).map(|n| node(at, n)).collect();
            let name = format!(
                "grown to {} at depth {}, paused at {height}",
                size * 2,
                at.len()
            );
            calls.partial(&name, ops, Some(options.clone()), pay);
        }
        size *= 2;
    }
    calls.get(grown[2], &[47]);
    // A first segment that deletes the element under the key of a tree's
    // root, and operations added after it that write into the same tree:
    // the tree is written on, with what it holds and the tree in it.
    let rooted: &[&[u8]] = &[b"documents", b"rooted"];
    let inner: &[&[u8]] = &[b"documents", b"rooted", b"z"];
    let mut ops = vec![insert(&rooted[..1], b"rooted", Element::empty_tree())];
    ops.extend((0..7).map(|n| node(rooted, n)));
    ops.push(insert(rooted, b"z", Element::empty_tree()));
    ops.push(insert(inner, b"deep", Element::new_item(vec![3; 30])));
    calls.batch("a tree with a tree in it", ops);
    for (round, height) in [None, Some(0u8), Some(1), Some(2)].into_iter().enumerate() {
        let tree = db.get(&rooted[..1], b"rooted", None, version).unwrap();
        let root = (tree.expect("the tree").root_key_and_tree_type_owned())
            .and_then(|(root, _)| root)
            .expect("a root key");
        assert_ne!(root, b"z");
        let options = BatchApplyOptions {
            batch_pause_height: height,
            ..BatchApplyOptions::default()
        };
        let added = [b'n', round as u8];
        calls.partial(
            &format!("the root of a tree deleted, paused at {height:?}"),
            vec![delete(rooted, &root)],
            Some(options),
            |cost| {
                Ok(vec![
                    insert(&[b"balances"], b"payer", charged(cost)),
                    insert(rooted, &added, Element::new_item(vec![5; 12])),
                ])
            },
        );
        calls.get(rooted, &added);
        calls.get(inner, b"deep");
    }
    // The same for a tree with one element: the first segment removes the
    // one node of its Merk, and what is added after it puts a node that
    // is new to that Merk, into the same tree.
    let solo: &[&[u8]] = &[b"documents", b"solo"];
    calls.batch(
        "a tree with one element",
        vec![
            insert(&solo[..1], b"solo", Element::empty_tree()),
            insert(solo, &[b'o', 0], Element::new_item(vec![6; 12])),
        ],
    );
    for (round, height) in [None, Some(0u8), Some(1), Some(2)].into_iter().enumerate() {
        let options = BatchApplyOptions {
            batch_pause_height: height,
            ..BatchApplyOptions::default()
        };
        let (only, next) = ([b'o', round as u8], [b'o', round as u8 + 1]);
        calls.partial(
            &format!("the one element of a tree deleted, paused at {height:?}"),
            vec![delete(solo, &only)],
            Some(options),
            |cost| {
                Ok(vec![
                    insert(&[b"balances"], b"payer", charged(cost)),
                    insert(solo, &next, Element::new_item(vec![7; 12])),
                ])
            },
        );
        calls.get(solo, &next);
        calls.refused(|calls| calls.get(solo, &only));
    }
    // The same for a tree that keeps its data flat: leaves appended by a
    // first segment that stops below the tree's element.
    calls.batch(
        "a tree of leaves",
        vec![insert(&documents[..2], b"log", Element::empty_mmr_tree())],
    );
    for height in [3u8, 2, 1] {
        let options = BatchApplyOptions {
            batch_pause_height: Some(height),
            ..BatchApplyOptions::default()
        };
        let log = path(&[b"documents", b"contract", b"log"]);
        let ops = vec![
            Op::mmr_tree_append_op(log.clone(), vec![height; 50]),
            Op::mmr_tree_append_op(log, vec![height + 1; 20]),
        ];
        calls.partial(
            &format!("leaves, paused at {height}"),
            ops,
            Some(options),
            pay,
        );
    }
    calls.get(&documents[..2], b"log");

    // A caller that fails, an addition that the batch refuses, and a batch
    // that is refused before the caller is asked.
    calls.refused(|calls| {
        calls.partial("the caller fails", vec![document(9, 70)], None, |_| {
            Err(Error::InvalidBatchOperation("the payer cannot pay"))
        });
    });
    calls.refused(|calls| {
        calls.partial("added under no tree", vec![document(9, 70)], None, |_| {
            Ok(vec![insert(
                &[b"nothing"],
                b"item",
                Element::new_item(vec![1]),
            )])
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
        insert(
            &[],
            b"documents",
            Element::empty_tree_with_flags(flags_of(0, 1)),
        ),
        insert(
            &documents[..1],
            b"contract",
            Element::empty_tree_with_flags(flags_of(0, 1)),
        ),
        insert(
            &[],
            b"balances",
            Element::empty_sum_tree_with_flags(flags_of(0, 1)),
        ),
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
                ReferencePathType::AbsolutePathReference(path(&[
                    documents[0],
                    documents[1],
                    b"of 2",
                ])),
                flags_of(5, 2),
            ),
        ),
        insert(
            documents,
            b"index",
            Element::empty_tree_with_flags(flags_of(5, 2)),
        ),
        insert(
            &[documents[0], documents[1], b"index"],
            b"entry",
            flagged(44, 5, 2),
        ),
    ];
    calls.flags_batch("a reference and a tree", ops);
    let ops = vec![
        insert(
            &[documents[0], documents[1], b"index"],
            b"entry",
            flagged(144, 6, 2),
        ),
        insert(
            &[documents[0], documents[1], b"index"],
            b"second",
            flagged(20, 6, 2),
        ),
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
        calls.flags_batch(
            "flags the callback cannot read",
            vec![insert(documents, b"of 1", unread)],
        );
    });
    calls.get(documents, b"of 1");

    // The partial twin: the payer is charged between the segments.
    let ops = vec![insert(documents, b"of 1", flagged(170, 7, 1))];
    calls.flags_partial("grown in epoch 7", ops, |cost| {
        Ok(vec![insert(&[b"balances"], b"payer", charged(cost))])
    });
    let ops = vec![insert(documents, b"of 2", flagged(15, 8, 2))];
    calls.flags_partial("shrunk in epoch 8", ops, |cost| {
        Ok(vec![insert(&[b"balances"], b"payer", charged(cost))])
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
    calls.flags_batch(
        "made in a transaction",
        vec![insert(documents, b"late", flagged(64, 9, 1))],
    );
    calls.flags_batch(
        "grown in it",
        vec![insert(documents, b"late", flagged(128, 10, 1))],
    );
    db.set_savepoint(&transaction).expect("a savepoint");
    calls.flags_batch(
        "to be undone",
        vec![insert(documents, b"late", flagged(8, 11, 1))],
    );
    db.rollback_to_savepoint(&transaction).expect("a rollback");
    calls.flags_batch(
        "shrunk in it",
        vec![insert(documents, b"late", flagged(70, 12, 1))],
    );
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

/// In how many ways a tree is written over, one for each kind of tree
/// in turn.
const WAYS: usize = 7;

/// Another element written over a tree that holds elements, with trees
/// in it: by a batch, by operations with no batch, by an insert and
/// inside write transactions. The tree is of every kind, and what takes
/// its place is an item, a tree of its own kind and a tree of another,
/// alone or with what is written into it, at once and later.
fn over_a_tree<E: Engine>(dir: &Path, version: &'static GroveVersion) -> Log {
    let db = open::<E>(dir);
    let made = made_by(version);
    let mut log = Log::default();
    let mut calls = Calls::new(&db, version, None, &mut log);
    let item = |len: usize| Element::new_item(vec![0xab; len]);
    // What is written into a tree that took the place of a filled one:
    // keys that the filled one had, and a tree in it under the key of one
    // that it had.
    let inside = |top: &[u8], kind: usize| {
        let nested = [top, b"nested".as_slice()];
        vec![
            insert(&[top], b"item", item(12)),
            insert(&[top], b"nested", (KINDS[kind].make)()),
            insert(&nested, b"b", item(7)),
            insert(&nested, b"deeper", Element::empty_tree()),
            insert(&[top, b"nested", b"deeper"], b"other", item(9)),
        ]
    };
    let fresh = |top: &[u8], kind: usize| {
        let mut ops = vec![insert(&[], top, (KINDS[kind].make)())];
        ops.extend(inside(top, kind));
        ops
    };
    // What is read of a tree that was written over: nothing under an
    // element that is no tree or under a tree nothing was written into,
    // and of a tree that was written into what was written, by its key
    // and by a proof of the whole tree. What the other reads give is
    // `where_a_tree_was`'s.
    fn held<E: Engine>(calls: &mut Calls<'_, '_, '_, E>, top: &[u8], written: bool) {
        let own = [top];
        let nested = [top, b"nested".as_slice()];
        let deeper = [top, b"nested".as_slice(), b"deeper"];
        calls.get(&[], top);
        if !written {
            calls.refused(|calls| calls.get(&own, b"long"));
            calls.refused(|calls| calls.get(&own, b"item"));
            return;
        }
        calls.get(&own, b"item");
        calls.get(&nested, b"b");
        calls.get(&deeper, b"other");
        if calls.through.is_none() {
            calls.proven(&own);
            calls.proven(&nested);
            calls.proven(&deeper);
        }
    }
    let allowed = crate::operations::insert::InsertOptions {
        validate_insertion_does_not_override_tree: false,
        ..Default::default()
    };

    for kind in 0..made {
        calls.batch("filled", filled(&[], &top(kind), kind, made));
    }
    for (kind, own) in KINDS.iter().enumerate().take(made) {
        let (top, next) = (top(kind), (kind + 1) % made);
        let top = top.as_slice();
        match kind % WAYS {
            // An item, then a tree over the item, then what is in it.
            0 => {
                calls.batch("an item over it", vec![insert(&[], top, item(5))]);
                held(&mut calls, top, false);
                calls.batch("a tree over the item", vec![insert(&[], top, (own.make)())]);
                held(&mut calls, top, false);
                calls.batch("into it", inside(top, kind));
            }
            // A tree of another kind with what is in it, in one batch.
            1 => calls.batch("a tree over it, written", fresh(top, next)),
            // A tree of its own kind alone, and what is in it later.
            2 => {
                calls.batch("a tree over it", vec![insert(&[], top, (own.make)())]);
                held(&mut calls, top, false);
                calls.batch("into it", inside(top, kind));
            }
            // A tree of another kind alone, and what is in it later.
            3 => {
                calls.batch(
                    "another tree over it",
                    vec![insert(&[], top, (KINDS[next].make)())],
                );
                held(&mut calls, top, false);
                calls.batch("into it", inside(top, next));
            }
            // With no batch: an item by an insert, and a tree over it.
            4 => {
                let under_the_root: &[&[u8]] = &[];
                let inserted = db.insert(
                    under_the_root,
                    top,
                    item(6),
                    Some(allowed.clone()),
                    None,
                    version,
                );
                calls
                    .log
                    .answer("insert an item over it".to_owned(), inserted);
                calls.root();
                held(&mut calls, top, false);
                calls.insert(&[], top, (own.make)());
                calls.batch("into it", inside(top, kind));
            }
            // By operations that are applied one by one.
            5 => {
                let options = Some(BatchApplyOptions::default());
                let ops = vec![insert(&[], top, item(5))];
                calls.unbatched_with("an item over it", ops, options.clone());
                held(&mut calls, top, false);
                calls.unbatched_with("a tree over the item, written", fresh(top, next), options);
            }
            // A tree in the tree: an item over it beside another write,
            // and a tree over the item with what is in it.
            _ => {
                let nested = [top, b"nested".as_slice()];
                let ops = vec![
                    insert(&[top], b"nested", item(4)),
                    insert(&[top], b"long", item(33)),
                ];
                calls.batch("an item over the tree in it", ops);
                calls.get(&[top], b"nested");
                calls.get(&[top], b"long");
                calls.refused(|calls| calls.get(&nested, b"b"));
                calls.batch("a tree over that item, written", inside(top, next));
                calls.get(&[top], b"long");
                calls.get(&[top], b"number");
            }
        }
        held(&mut calls, top, true);
    }

    // Inside a write transaction that is rolled back, and in one that
    // commits.
    let (rolled, kept): (&[u8], &[u8]) = (b"rolled back", b"behind a savepoint");
    calls.batch("filled", filled(&[], rolled, 0, made));
    calls.batch("filled", filled(&[], kept, 1, made));
    drop(calls);
    let transaction = db.start_transaction();
    let mut calls = Calls::new(&db, version, Some(&transaction), &mut log);
    calls.batch("an item over it", vec![insert(&[], rolled, item(5))]);
    held(&mut calls, rolled, false);
    drop(calls);
    db.rollback_transaction(&transaction).expect("a rollback");
    drop(transaction);
    let mut calls = Calls::new(&db, version, None, &mut log);
    calls.root();
    calls.get(&[rolled], b"long");
    calls.get(&[rolled, b"nested"], b"a");
    calls.get(&[rolled, b"nested", b"deeper"], b"deep");
    drop(calls);

    let transaction = db.start_transaction();
    let mut calls = Calls::new(&db, version, Some(&transaction), &mut log);
    calls.batch("a tree over it, written", fresh(rolled, 2));
    held(&mut calls, rolled, true);
    // A savepoint, and behind it a tree that is written over: the
    // rollback brings back what it held.
    calls.batch("written before", vec![insert(&[kept], b"before", item(3))]);
    db.set_savepoint(&transaction).expect("a savepoint");
    calls.batch("a tree over it, written", fresh(kept, 3));
    held(&mut calls, kept, true);
    db.rollback_to_savepoint(&transaction).expect("a rollback");
    calls.root();
    calls.get(&[kept], b"before");
    calls.get(&[kept], b"long");
    calls.get(&[kept, b"nested"], b"a");
    calls.get(&[kept, b"nested", b"deeper"], b"deep");
    calls.batch("an item over it", vec![insert(&[], kept, item(5))]);
    held(&mut calls, kept, false);
    drop(calls);
    let committed = db.commit_transaction(transaction);
    log.answer("commit".to_owned(), committed);
    let mut calls = Calls::new(&db, version, None, &mut log);
    calls.root();
    held(&mut calls, rolled, true);
    held(&mut calls, kept, false);
    // Every tree that took the place of another is written into again.
    let ops = (0..made).map(|kind| insert(&[top(kind).as_slice()], b"after", item(20 + kind)));
    calls.batch("after it all", ops.collect());
    db.flush().expect("a flush");
    log
}

fn written_over_a_tree_on(version: &'static GroveVersion) {
    let made = made_by(version);
    let log = on_both_engines!(over_a_tree, version);
    // Every way was gone, and the trees behind them were of every kind.
    assert!(made >= WAYS);
    let batches = |name: &str| {
        let named = format!("batch {name}");
        log.answers
            .iter()
            .filter(|(call, ..)| *call == named)
            .count()
    };
    assert_eq!(batches("filled"), made + 2);
    assert_eq!(batches("after it all"), 1);
    assert!(log
        .answers
        .last()
        .is_some_and(|(_, _, root)| root.starts_with("Ok")));
}

#[test]
fn another_element_over_a_tree_on_grove_v1() {
    written_over_a_tree_on(&GROVE_VERSIONS[0]);
}

#[test]
fn another_element_over_a_tree_on_grove_v2() {
    written_over_a_tree_on(&GROVE_VERSIONS[1]);
}

#[test]
fn another_element_over_a_tree_on_grove_v3() {
    written_over_a_tree_on(&GROVE_VERSIONS[2]);
}

#[test]
fn another_element_over_a_tree_on_grove_v4() {
    written_over_a_tree_on(&GROVE_VERSIONS[3]);
}

#[test]
fn another_element_over_a_tree_on_grove_v5() {
    written_over_a_tree_on(&GROVE_VERSIONS[4]);
}

/// Trees that keep their data flat among what is written over and what
/// takes the place of a tree, a partial batch that writes over a tree, and
/// a batch that writes over a tree and over a tree in it.
fn over_flat_trees<E: Engine>(dir: &Path, version: &'static GroveVersion) -> Log {
    let db = open::<E>(dir);
    let made = made_by(version);
    let mut log = Log::default();
    let mut calls = Calls::new(&db, version, None, &mut log);
    let item = || Element::new_item(vec![0x5a; 9]);
    let root: &[&[u8]] = &[];
    // A leaf appended to the tree under `key`, and what the tree answers.
    let append = |calls: &mut Calls<'_, '_, '_, E>, key: &[u8], leaf: u8| {
        let appended = db.mmr_tree_append(root, key, vec![leaf; 40], None, version);
        calls.log.answer("append".to_owned(), appended);
        calls.root();
        let count = db.mmr_tree_leaf_count(root, key, None, version);
        calls.log.answer("count".to_owned(), count);
        let hash = db.mmr_tree_root_hash(root, key, None, version);
        calls.log.answer("root of the leaves".to_owned(), hash);
    };
    for key in [b"m1".as_slice(), b"m2", b"m3", b"m4"] {
        calls.batch(
            "a tree of leaves",
            vec![insert(&[], key, Element::empty_mmr_tree())],
        );
        for leaf in 1..=3 {
            append(&mut calls, key, leaf);
        }
    }
    for key in [b"t1".as_slice(), b"t2", b"t3"] {
        calls.batch("filled", filled(&[], key, 0, made));
    }
    // Trees that were written into and emptied: entered, with no node.
    for key in [b"e1".as_slice(), b"e2"] {
        let ops = vec![
            insert(&[], key, Element::empty_tree()),
            insert(&[key], b"x", item()),
        ];
        calls.batch("a tree with an element", ops);
        calls.batch("emptied", vec![delete(&[key], b"x")]);
    }

    // An item over a tree of leaves, a tree over the item, and an element
    // in it.
    calls.batch("an item over leaves", vec![insert(&[], b"m1", item())]);
    calls.get(&[], b"m1");
    let ops = vec![
        insert(&[], b"m1", Element::empty_tree()),
        insert(&[b"m1"], b"x", item()),
    ];
    calls.batch("a tree over the item, written", ops);
    calls.get(&[b"m1"], b"x");
    calls.proven(&[b"m1"]);
    // A tree over a tree of leaves, alone, and an element in it later.
    calls.batch(
        "a tree over leaves",
        vec![insert(&[], b"m2", Element::empty_count_tree())],
    );
    calls.batch("into it", vec![insert(&[b"m2"], b"x", item())]);
    calls.get(&[], b"m2");
    calls.proven(&[b"m2"]);
    // A tree of leaves over a tree with elements, and leaves in it later.
    calls.batch(
        "leaves over a tree",
        vec![insert(&[], b"t1", Element::empty_mmr_tree())],
    );
    calls.get(&[], b"t1");
    for leaf in 4..=5 {
        append(&mut calls, b"t1", leaf);
    }
    // A tree of leaves over a tree of leaves: nothing tells the new one
    // from the old, and its records stay on both engines.
    calls.batch(
        "leaves over leaves",
        vec![insert(&[], b"m3", Element::empty_mmr_tree())],
    );
    append(&mut calls, b"m3", 6);
    // A tree that sums over a tree of leaves, with a number in it.
    let ops = vec![
        insert(&[], b"m4", Element::empty_sum_tree()),
        insert(&[b"m4"], b"five", Element::new_sum_item(5)),
    ];
    calls.batch("a sum tree over leaves, written", ops);
    calls.get(&[], b"m4");
    calls.get(&[b"m4"], b"five");
    calls.proven(&[b"m4"]);

    // A tree of another type over a tree that was emptied: with a
    // number in it, and alone with an element in it later.
    let ops = vec![
        insert(&[], b"e1", Element::empty_sum_tree()),
        insert(&[b"e1"], b"five", Element::new_sum_item(5)),
    ];
    calls.batch("a sum tree over an emptied tree, written", ops);
    calls.get(&[b"e1"], b"five");
    calls.batch(
        "a count tree over an emptied tree",
        vec![insert(&[], b"e2", Element::empty_count_tree())],
    );
    calls.batch("into it", vec![insert(&[b"e2"], b"x", item())]);
    calls.get(&[], b"e2");

    // A partial batch writes an item over a tree: its two segments are
    // one group.
    calls.partial(
        "an item over a tree",
        vec![insert(&[], b"t2", item())],
        None,
        |_| Ok(Vec::new()),
    );
    calls.get(&[], b"t2");
    calls.refused(|calls| calls.get(&[b"t2"], b"item"));
    // One batch writes an item over a tree and an item over a tree in it.
    let options = BatchApplyOptions {
        disable_operation_consistency_check: true,
        ..Default::default()
    };
    let ops = vec![
        insert(&[b"t3"], b"nested", item()),
        insert(&[], b"t3", item()),
    ];
    calls.refused(|calls| {
        calls.batch_with(
            "an item over a tree and over a tree in it",
            ops,
            Some(options),
        );
    });
    calls.get(&[], b"t3");
    calls.batch(
        "after it all",
        vec![
            insert(&[b"m1"], b"y", item()),
            insert(&[b"m2"], b"y", item()),
        ],
    );
    db.flush().expect("a flush");
    log
}

/// The history of `over_flat_trees` on every grove version: the same
/// answers, costs and root hashes on both engines and a sound LigninDB
/// database. Every append is taken. The batch that writes over a tree
/// and over a tree in it is refused by GroveDB on both engines, so no
/// group names two trees, one in the other, as written over.
#[test]
fn trees_that_keep_their_data_flat_are_written_over_and_written_over_trees() {
    for version in GROVE_VERSIONS.iter() {
        let log = on_both_engines!(over_flat_trees, version);
        let appended = log.answers.iter().filter(|(call, ..)| call == "append");
        let appended: Vec<bool> = appended
            .map(|(.., answer)| answer.starts_with("Ok"))
            .collect();
        assert_eq!(appended, [true; 15]);
        let batches = log
            .answers
            .iter()
            .filter(|(call, ..)| call.starts_with("batch "));
        let refused: Vec<&str> = batches
            .filter(|(.., answer)| !answer.starts_with("Ok"))
            .map(|(call, ..)| call.as_str())
            .collect();
        assert_eq!(refused, ["batch an item over a tree and over a tree in it"]);
    }
}

/// Once the history of `over_flat_trees` is applied, the subtree
/// directory of LigninDB has each tree that took the place of another
/// as a tree of its element's type: a tree that keeps its data flat where
/// a Merk was, a Merk where such a tree was, and a Merk of another type
/// where a tree was entered that has no node.
#[test]
fn the_directory_has_a_tree_that_took_the_place_of_another_by_its_own_type() {
    for version in GROVE_VERSIONS.iter() {
        let dir = tempfile::tempdir().expect("a directory");
        over_flat_trees::<LigninEngine>(dir.path(), version);
        let db = open::<LigninEngine>(dir.path());
        let engine = db.engine();
        let under_the_root: &[&[u8]] = &[];
        let mut entered = 0;
        for key in [b"m1".as_slice(), b"m2", b"m3", b"m4", b"t1", b"e1", b"e2"] {
            let element = db.get(under_the_root, key, None, version).unwrap();
            let tree_type = (element.expect("an element").tree_type()).expect("a tree");
            let tx = engine.start_read_transaction();
            let handle = engine.subtree([key].as_ref().into(), None, &tx).unwrap();
            let entry = handle.entry().expect("the directory answers");
            let entry = entry.unwrap_or_else(|| panic!("{} is entered", hex::encode(key)));
            assert_eq!(entry.tree_type().expect("a tree type"), tree_type);
            entered += 1;
        }
        assert_eq!(entered, 7);
        drop(db);
        check(dir.path(), version);
    }
}

/// Trees written over trees by batches that also insert into the new
/// tree only what is not there. Such an insert asks the store for its
/// key, and until the batch is applied the store holds what the tree
/// that was there had under it, on both engines: under such a key the
/// insert fails its batch or is left out, as its kind has it, and under
/// any other it is made.
fn only_what_is_not_there<E: Engine>(dir: &Path, version: &'static GroveVersion) -> Log {
    let db = open::<E>(dir);
    let made = made_by(version);
    let mut log = Log::default();
    let mut calls = Calls::new(&db, version, None, &mut log);
    let item = || Element::new_item(vec![0x77; 7]);
    let absent =
        |at: &[&[u8]], key: &[u8]| Op::insert_if_not_exists_op(path(at), key.to_vec(), item());
    // The same insert as one that is left out where its key holds a
    // record, with no refusal.
    let or_skipped = |at: &[&[u8]], key: &[u8]| {
        Op::insert_if_not_exists_or_skip_op(path(at), key.to_vec(), item())
    };
    for top in [b"one".as_slice(), b"two", b"three", b"four", b"five"] {
        calls.batch("filled", filled(&[], top, 0, made));
    }
    // Under each key that the old tree had, the key of its root among
    // them, the insert finds a record and fails its batch.
    let old: [&[u8]; 7] = [
        b"item", b"long", b"number", b"nested", b"never", b"emptied", b"inside",
    ];
    for key in old {
        let ops = vec![
            insert(&[], b"one", Element::empty_tree()),
            absent(&[b"one"], key),
            insert(&[b"one"], b"other", item()),
        ];
        calls.refused(|calls| calls.batch("a tree over one, and what is there", ops));
    }
    calls.get(&[b"one"], b"long");
    // Under a key that the old tree had not, beside another write.
    let ops = vec![
        insert(&[], b"one", Element::empty_tree()),
        absent(&[b"one"], b"new"),
        insert(&[b"one"], b"other", item()),
    ];
    calls.batch("a tree over one, and what is not there", ops);
    calls.get(&[b"one"], b"new");
    calls.get(&[b"one"], b"other");
    calls.proven(&[b"one"]);
    // A tree of another type, with a number beside.
    let ops = vec![
        insert(&[], b"two", Element::empty_sum_tree()),
        absent(&[b"two"], b"new"),
        insert(&[b"two"], b"other", Element::new_sum_item(4)),
    ];
    calls.batch("a sum tree over two, and what is not there", ops);
    calls.get(&[b"two"], b"other");
    calls.proven(&[b"two"]);
    // The same with nothing else written into the new tree.
    let ops = vec![
        insert(&[], b"three", Element::empty_tree()),
        absent(&[b"three"], b"new"),
    ];
    calls.batch("a tree over three, and what is not there", ops);
    calls.proven(&[b"three"]);
    // Inserts that are left out under every key the old tree had, beside
    // one under a key it had not and another write: the new tree holds
    // those two.
    let mut ops = vec![insert(&[], b"four", Element::empty_tree())];
    ops.extend(old.iter().map(|key| or_skipped(&[b"four"], key)));
    ops.push(or_skipped(&[b"four"], b"new"));
    ops.push(insert(&[b"four"], b"other", item()));
    calls.batch("a tree over four, and what is there left out", ops);
    calls.get(&[b"four"], b"new");
    calls.get(&[b"four"], b"other");
    calls.proven(&[b"four"]);
    // All of them left out, and nothing else written: the new tree has no
    // element, and takes one later.
    let mut ops = vec![insert(&[], b"five", Element::empty_tree())];
    ops.extend(old.iter().map(|key| or_skipped(&[b"five"], key)));
    calls.batch("a tree over five, and all of it left out", ops);
    calls.proven(&[b"five"]);
    calls.batch("into it", vec![insert(&[b"five"], b"later", item())]);
    calls.proven(&[b"five"]);
    // A partial batch whose first segment leaves the insert out, and
    // whose caller then puts an element under the same key: the writer of
    // the new tree asked the store for that key and began with no node.
    calls.batch("filled", filled(&[], b"six", 0, made));
    let options = BatchApplyOptions {
        batch_pause_height: Some(0),
        ..BatchApplyOptions::default()
    };
    let ops = vec![
        insert(&[], b"six", Element::empty_sum_tree()),
        or_skipped(&[b"six"], b"number"),
    ];
    calls.partial(
        "a sum tree over six, its insert left out",
        ops,
        Some(options),
        |_| Ok(vec![insert(&[b"six"], b"number", Element::new_sum_item(6))]),
    );
    calls.get(&[], b"six");
    calls.get(&[b"six"], b"number");
    calls.proven(&[b"six"]);
    let after = |top: &[u8]| insert(&[top], b"after", item());
    calls.batch("after it all", vec![after(b"one"), after(b"three")]);
    calls.batch(
        "after it all",
        vec![insert(&[b"two"], b"after", Element::new_sum_item(1))],
    );
    calls.proven(&[b"one"]);
    calls.proven(&[b"two"]);
    calls.proven(&[b"three"]);
    db.flush().expect("a flush");
    log
}

/// The history of `only_what_is_not_there` on every grove version: the
/// same answers, costs and root hashes on both engines, and a sound
/// LigninDB database whose directory has each new tree by its own type.
#[test]
fn a_tree_over_a_tree_takes_what_is_not_there_as_on_rocksdb() {
    for version in GROVE_VERSIONS.iter() {
        let dir = tempfile::tempdir().expect("a directory");
        let log = only_what_is_not_there::<LigninEngine>(dir.path(), version);
        #[cfg(feature = "rocksdb_storage")]
        {
            let dir = tempfile::tempdir().expect("a directory");
            let on_rocksdb = only_what_is_not_there::<crate::RocksDbEngine>(dir.path(), version);
            assert_same(&log, &on_rocksdb);
        }
        log.assert_refusals();
        let db = open::<LigninEngine>(dir.path());
        let engine = db.engine();
        // Each with the type of the tree that the batch of one segment
        // made. What the element of a tree is after a partial batch wrote
        // it in both segments is GroveDB's to say, the same on both
        // engines: the directory has the tree by its element's type.
        let under_the_root: &[&[u8]] = &[];
        for (key, made) in [
            (b"one".as_slice(), Some(TreeType::NormalTree)),
            (b"two", Some(TreeType::SumTree)),
            (b"three", Some(TreeType::NormalTree)),
            (b"four", Some(TreeType::NormalTree)),
            (b"five", Some(TreeType::NormalTree)),
            (b"six", None),
        ] {
            let named = String::from_utf8_lossy(key);
            let element = db.get(under_the_root, key, None, version).unwrap();
            let tree_type = (element.expect("an element").tree_type()).expect("a tree");
            assert!(made.is_none_or(|made| made == tree_type), "{named}");
            let tx = engine.start_read_transaction();
            let handle = engine.subtree([key].as_ref().into(), None, &tx).unwrap();
            let entry = handle.entry().expect("the directory answers");
            let entry = entry.unwrap_or_else(|| panic!("{named} is entered"));
            let entered = entry.tree_type().expect("a tree type");
            assert_eq!(entered, tree_type, "{named}");
            // Nothing of the tree that was there is under the new one.
            let nested: [&[u8]; 2] = [key, b"nested"];
            let below = engine.subtree(nested.as_ref().into(), None, &tx).unwrap();
            let below = below.entry().expect("the directory answers");
            assert_eq!(below, None, "{named}");
        }
        drop(db);
        check(dir.path(), version);
    }
}

/// What the two engines answer differently once a tree was written over
/// (D-133, D-227). On RocksDB the records of the old tree are still under
/// its prefix, and so are those of the trees it held under theirs: a tree
/// made at that path stands on them, and a path that leads through an
/// element of the old tree still finds it by its key. On LigninDB the old
/// tree's Merk left with its element, and the Merks below with it, until
/// `GROVE-03-F1` keeps their nodes as records that no tree links.
///
/// Gives the log of the calls that both engines answer alike, and apart
/// from it each of the other reads with what it gave.
fn where_a_tree_was<E: Engine>(
    dir: &Path,
    version: &'static GroveVersion,
) -> (Log, Vec<(String, OperationCost, String)>) {
    let db = open::<E>(dir);
    let made = made_by(version);
    let (mut log, mut apart) = (Log::default(), Log::default());
    let mut calls = Calls::new(&db, version, None, &mut log);
    let mut reads = Calls::new(&db, version, None, &mut apart);
    let item = Element::new_item(vec![0xcd; 8]);
    for top in [b"one".as_slice(), b"two", b"three", b"four"] {
        calls.batch("filled", filled(&[], top, 0, made));
    }
    // A tree over a tree, alone.
    calls.batch(
        "a tree over one",
        vec![insert(&[], b"one", Element::empty_tree())],
    );
    reads.is_empty(&[b"one"]);
    reads.get(&[b"one", b"nested"], b"b");
    reads.get(&[b"one", b"nested", b"deeper"], b"deep");
    // An item over a tree, a tree over the item, and in it a tree under
    // the key of one that the old tree held.
    calls.batch("an item over two", vec![insert(&[], b"two", item.clone())]);
    reads.get(&[b"two", b"nested"], b"b");
    reads.get(&[b"two", b"nested", b"deeper"], b"deep");
    let ops = vec![
        insert(&[], b"two", Element::empty_tree()),
        insert(&[b"two"], b"nested", Element::empty_tree()),
    ];
    calls.batch("a tree over the item", ops);
    calls.refused(|calls| calls.get(&[b"two", b"nested"], b"b"));
    calls.proven(&[b"two"]);
    reads.all(&[b"two"]);
    reads.is_empty(&[b"two"]);
    reads.is_empty(&[b"two", b"nested"]);
    reads.get(&[b"two", b"nested", b"deeper"], b"deep");
    // A tree over a tree with a tree in it and a tree in that, in one
    // batch.
    let ops = vec![
        insert(&[], b"three", Element::empty_tree()),
        insert(&[b"three"], b"nested", Element::empty_tree()),
        insert(&[b"three", b"nested"], b"deeper", Element::empty_tree()),
    ];
    calls.batch("a tree over three, written", ops);
    reads.is_empty(&[b"three", b"nested"]);
    calls.refused(|calls| calls.get(&[b"three", b"nested"], b"b"));
    calls.refused(|calls| calls.get(&[b"three", b"nested", b"deeper"], b"deep"));
    reads.is_empty(&[b"three", b"nested", b"deeper"]);
    // A tree over a tree with elements in it, under keys the old tree had
    // and under others: a read by a key that only the old tree had.
    let ops = vec![
        insert(&[], b"four", Element::empty_tree()),
        insert(&[b"four"], b"item", item.clone()),
        insert(&[b"four"], b"other", item.clone()),
        insert(&[b"four"], b"nested", Element::empty_tree()),
        insert(&[b"four", b"nested"], b"b", item.clone()),
        insert(&[b"four", b"nested"], b"c", item.clone()),
    ];
    calls.batch("a tree over four, written", ops);
    calls.get(&[b"four"], b"item");
    calls.get(&[b"four", b"nested"], b"b");
    calls.proven(&[b"four"]);
    calls.proven(&[b"four", b"nested"]);
    reads.all(&[b"four"]);
    reads.all(&[b"four", b"nested"]);
    for key in [
        b"long".as_slice(),
        b"number",
        b"inside",
        b"emptied",
        b"never",
    ] {
        reads.get(&[b"four"], key);
    }
    reads.get(&[b"four", b"nested"], b"a");
    reads.get(&[b"four", b"nested", b"deeper"], b"deep");
    // Two writes that read first: each with whether it changed the root.
    let root = || db.root_hash(None, version).unwrap().expect("a root");
    let moved = |before: [u8; 32]| (OperationCost::default(), (root() != before).to_string());
    // An insert of what is not there asks the store for its key, and is
    // refused where the store holds a record under it.
    let before = root();
    let absent = Op::insert_if_not_exists_op(path(&[b"four"]), b"long".to_vec(), item.clone());
    let applied = db.apply_batch(vec![absent], None, None, version);
    reads.log.answer("absent/four/long".to_owned(), applied);
    let (cost, changed) = moved(before);
    (reads.log.answers).push(("root/absent/four/long".to_owned(), cost, changed));
    // The delete of a tree asks whether it is empty.
    let before = root();
    let under_the_root: &[&[u8]] = &[];
    let deleted = db.delete(under_the_root, b"one", None, None, version);
    reads.log.answer("delete/one".to_owned(), deleted);
    let (cost, changed) = moved(before);
    (reads.log.answers).push(("root/delete/one".to_owned(), cost, changed));
    db.flush().expect("a flush");
    let gave = |(call, cost, answer): &(String, OperationCost, String)| {
        let found = answer.starts_with("Ok");
        let gave = match call.split('/').next() {
            Some("empty" | "root") => answer.clone(),
            Some("all") => answer
                .matches("KeyElementPairResultItem")
                .count()
                .to_string(),
            Some("delete" | "absent") => String::from(if found { "made" } else { "refused" }),
            _ => String::from(if found { "found" } else { "nothing" }),
        };
        (call.clone(), cost.clone(), gave)
    };
    (log, apart.answers.iter().map(gave).collect())
}

/// The reads of `where_a_tree_was` that the engines answer differently,
/// each with the answer that is required, which is RocksDB's, and with
/// the one LigninDB gives until `GROVE-03-F1` closes the difference
/// (D-227): whether a tree is empty, how many elements a query finds in a
/// range, whether a key is read, whether an insert of what is not there
/// is made, and whether the delete of a tree is made.
/// They are the acceptance list of that task: once the nodes of a tree
/// that was written over are kept as records that no tree links, LigninDB
/// gives the second column, and the third goes.
const READS_WHERE_A_TREE_WAS: [(&str, &str, &str); 24] = [
    // A tree nothing was written into, over a tree with elements.
    ("empty/one", "Ok(false)", "Ok(true)"),
    ("get/one/nested/b", "found", "nothing"),
    ("get/one/nested/deeper/deep", "found", "nothing"),
    // An item over a tree: the trees that the old tree held.
    ("get/two/nested/b", "found", "nothing"),
    ("get/two/nested/deeper/deep", "found", "nothing"),
    // A tree over that item with one tree in it.
    ("all/two", "7", "1"),
    ("empty/two", "Ok(false)", "Ok(false)"),
    ("empty/two/nested", "Ok(false)", "Ok(true)"),
    ("get/two/nested/deeper/deep", "found", "nothing"),
    // A tree over a tree, with a tree in it and a tree in that.
    ("empty/three/nested", "Ok(false)", "Ok(false)"),
    ("empty/three/nested/deeper", "Ok(false)", "Ok(true)"),
    // A tree over a tree, with three elements and two in a tree in it.
    ("all/four", "8", "3"),
    ("all/four/nested", "4", "2"),
    ("get/four/long", "found", "nothing"),
    ("get/four/number", "found", "nothing"),
    ("get/four/inside", "found", "nothing"),
    ("get/four/emptied", "found", "nothing"),
    ("get/four/never", "found", "nothing"),
    ("get/four/nested/a", "found", "nothing"),
    ("get/four/nested/deeper/deep", "found", "nothing"),
    // An insert of what is not there, under a key the old tree had: a
    // write.
    ("absent/four/long", "refused", "made"),
    ("root/absent/four/long", "false", "true"),
    // The tree nothing was written into is deleted: a write.
    ("delete/one", "refused", "made"),
    ("root/delete/one", "false", "true"),
];

/// A known difference, and no property of the engine (D-227,
/// `GROVE-03-F1`): where a tree was written over, RocksDB still reads the
/// records of the old tree, and LigninDB, which dropped its Merk, does
/// not. RocksDB's answers are the required ones and are held here on
/// RocksDB. What LigninDB answers today is held beside them so that the
/// difference is the one that is written down, neither more nor less:
/// 20 reads with another answer, 2 with the same answer, which the
/// engines are compared on for their cost and differ in it, and for each
/// of the two reads that are writes whether the root hash changed with
/// it. Every other call of the history answers and costs the same.
#[test]
fn lignindb_does_not_yet_read_what_rocksdb_reads_where_a_tree_was_written_over() {
    let answers = |required: bool| -> Vec<(String, String)> {
        let reads = READS_WHERE_A_TREE_WAS.iter();
        let gave = |&(read, rocksdb, lignindb): &(&str, &str, &str)| {
            let gave = if required { rocksdb } else { lignindb };
            (read.to_owned(), gave.to_owned())
        };
        reads.map(gave).collect()
    };
    let differ = READS_WHERE_A_TREE_WAS
        .iter()
        .filter(|read| read.1 != read.2);
    assert_eq!(differ.count(), READS_WHERE_A_TREE_WAS.len() - 2);
    let gave = |read: &[(String, OperationCost, String)]| -> Vec<(String, String)> {
        let read = read.iter();
        read.map(|(call, _, gave)| (call.clone(), gave.clone()))
            .collect()
    };
    for version in GROVE_VERSIONS.iter() {
        let dir = tempfile::tempdir().expect("a directory");
        let (log, read) = where_a_tree_was::<LigninEngine>(dir.path(), version);
        log.assert_refusals();
        // The known difference: nothing of the old trees is read.
        assert_eq!(gave(&read), answers(false));
        check(dir.path(), version);
        #[cfg(feature = "rocksdb_storage")]
        {
            let dir = tempfile::tempdir().expect("a directory");
            let (on_rocksdb, required) =
                where_a_tree_was::<crate::RocksDbEngine>(dir.path(), version);
            assert_same(&log, &on_rocksdb);
            // The required answers: each tree stands on the records of
            // the one that was there.
            assert_eq!(gave(&required), answers(true));
            // A read that both answer alike is in the list for its cost.
            let mut alike = 0;
            for (lignin, rocksdb) in read.iter().zip(&required) {
                if lignin.2 == rocksdb.2 {
                    assert_ne!(lignin.1, rocksdb.1, "{}", lignin.0);
                    alike += 1;
                }
            }
            assert_eq!(alike, 2);
        }
    }
}

/// What a batch meets that keeps the value of a node which a commit
/// outside its transaction deleted between the batch's read of the node
/// and its application. The caller of a partial batch is called between
/// the two, and has the commit made there.
///
/// Gives the answer of the batch, that of the batch made again through
/// the same transaction where the first was refused as a conflict, that
/// of the commit, and whether the deleted element and the batch's own are
/// read afterwards.
fn taken_between<E: Engine>(
    dir: &Path,
    version: &'static GroveVersion,
) -> (
    Result<(), Error>,
    Option<Result<(), Error>>,
    Result<(), Error>,
    [bool; 2],
) {
    let db = open::<E>(dir);
    let held: &[&[u8]] = &[b"top", b"held"];
    let numbered = |n: u8| insert(held, &[n], Element::new_item(vec![n; 20]));
    let mut ops = vec![
        insert(&[], b"top", Element::empty_tree()),
        insert(&[b"top"], b"held", Element::empty_tree()),
    ];
    ops.extend((1..=7).map(numbered));
    (db.apply_batch(ops, None, None, version).unwrap()).expect("the tree is made");

    let transaction = db.start_transaction();
    // The eighth element goes under the seventh: the batch writes the
    // seventh's node again, with the value it read.
    let between = |_: &OperationCost, _: &_| {
        let deleted = db.delete(held, &[7], None, None, version);
        deleted
            .unwrap()
            .expect("the delete outside the transaction");
        Ok(Vec::new())
    };
    let applied = db
        .apply_partial_batch(
            vec![numbered(8)],
            None,
            between,
            Some(&transaction),
            version,
        )
        .unwrap();
    let conflict = applied.as_ref().is_err_and(Error::is_commit_conflict);
    let again = conflict.then(|| {
        // The transaction is open and as it was: it reads, and takes the
        // batch that is made again.
        let read = db.get(&[b"top".as_slice()], b"held", Some(&transaction), version);
        read.unwrap().expect("a read through the transaction");
        (db.apply_batch(vec![numbered(8)], None, Some(&transaction), version)).unwrap()
    });
    let committed = db.commit_transaction(transaction).unwrap();
    let read = [7, 8].map(|n: u8| db.get(held, &[n], None, version).unwrap().is_ok());
    db.flush().expect("a flush");
    (applied, again, committed, read)
}

#[test]
fn a_commit_that_takes_a_value_a_batch_keeps_is_a_conflict_at_the_batch() {
    for version in GROVE_VERSIONS.iter() {
        let dir = tempfile::tempdir().expect("a directory");
        let (applied, again, committed, read) = taken_between::<LigninEngine>(dir.path(), version);
        // The batch is refused as a commit that lost is, and the same
        // batch made again is taken: the delete stands, and the write.
        let refused = applied.expect_err("the batch is refused");
        assert!(refused.is_commit_conflict(), "{refused:?}");
        let conflict = grovedb_storage::error::CommitError::Conflict;
        assert_eq!(refused.commit_error(), Some(conflict));
        assert!(matches!(again, Some(Ok(()))), "{again:?}");
        assert!(committed.is_ok(), "{committed:?}");
        assert_eq!(read, [false, true]);
        check(dir.path(), version);
        // RocksDB takes the batch and the commit: the transaction wrote
        // its records after the commit outside, which is not a conflict
        // there, and the deleted element is back.
        #[cfg(feature = "rocksdb_storage")]
        {
            let dir = tempfile::tempdir().expect("a directory");
            let (applied, again, committed, read) =
                taken_between::<crate::RocksDbEngine>(dir.path(), version);
            assert!(applied.is_ok() && again.is_none(), "{applied:?}");
            assert!(committed.is_ok(), "{committed:?}");
            assert_eq!(read, [true, true]);
        }
    }
}

/// An item that takes backward references.
fn target(value: &[u8]) -> Element {
    Element::new_item_allowing_bidirectional_references(value.to_vec())
}

/// A reference that its target keeps a backward reference to.
fn bidirectional(to: ReferencePathType, cascade_on_update: bool) -> Element {
    let reference = BidirectionalReference {
        forward_reference_path: to,
        backward_references: Vec::new(),
        cascade_on_update,
        max_hop: None,
    };
    Element::BidirectionalReference(reference, None)
}

fn beside(key: &[u8]) -> ReferencePathType {
    ReferencePathType::SiblingReference(key.to_vec())
}

fn at(to: &[&[u8]]) -> ReferencePathType {
    ReferencePathType::AbsolutePathReference(path(to))
}

fn bidirectional_references<E: Engine>(dir: &Path, version: &'static GroveVersion) -> Log {
    let db = open::<E>(dir);
    let mut log = Log::default();
    let mut calls = Calls::new(&db, version, None, &mut log);
    let far: &[&[u8]] = &[b"far", b"farther", b"farthest"];
    let ops = vec![
        insert(&[], b"near", Element::empty_tree()),
        insert(&[], b"sums", Element::empty_sum_tree()),
        insert(&[], b"far", Element::empty_tree()),
        insert(&far[..1], b"farther", Element::empty_count_tree()),
        insert(&far[..2], b"farthest", Element::empty_tree()),
        insert(&[b"near"], b"plain", Element::new_item(vec![1; 10])),
    ];
    calls.batch("trees", ops);

    // A target, then a chain of two references to it, each in a batch of
    // its own, and a reference from three trees down.
    calls.batch(
        "a target",
        vec![insert(&[b"near"], b"value", target(b"hello"))],
    );
    calls.batch(
        "a reference",
        vec![insert(
            &[b"near"],
            b"r1",
            bidirectional(beside(b"value"), true),
        )],
    );
    calls.batch(
        "a reference to it",
        vec![insert(
            &[b"near"],
            b"r2",
            bidirectional(beside(b"r1"), true),
        )],
    );
    let ops = vec![insert(
        far,
        b"from afar",
        bidirectional(at(&[b"near", b"value"]), true),
    )];
    calls.batch("a reference from afar", ops);
    for key in [b"value".as_slice(), b"r1", b"r2"] {
        calls.get(&[b"near"], key);
    }
    calls.get(far, b"from afar");
    calls.verify();

    // The target is written again: every reference of the chain follows.
    calls.batch(
        "the target again",
        vec![insert(&[b"near"], b"value", target(b"a longer value"))],
    );
    calls.get(&[b"near"], b"r2");
    calls.get(far, b"from afar");
    // The same reference again changes nothing, and a reference is
    // pointed at another target.
    calls.batch(
        "the same reference",
        vec![insert(
            &[b"near"],
            b"r1",
            bidirectional(beside(b"value"), true),
        )],
    );
    calls.batch(
        "another target",
        vec![insert(&[b"near"], b"second", target(b"second"))],
    );
    calls.batch(
        "pointed at it",
        vec![insert(
            &[b"near"],
            b"r2",
            bidirectional(beside(b"second"), true),
        )],
    );
    calls.get(&[b"near"], b"r2");
    calls.verify();

    // A target, a reference to it and a reference to that one, made by one
    // batch in a tree that sums, and a plain reference to a target.
    let ops = vec![
        insert(&[b"sums"], b"r2", bidirectional(beside(b"r1"), true)),
        insert(&[b"sums"], b"r1", bidirectional(beside(b"value"), true)),
        insert(
            &[b"sums"],
            b"value",
            Element::new_item_with_sum_item_allowing_bidirectional_references(b"pay".to_vec(), 5),
        ),
        insert(&[b"sums"], b"other", Element::new_sum_item(7)),
    ];
    calls.batch("a chain in one batch", ops);
    calls.get(&[b"sums"], b"r2");
    calls.get(&[], b"sums");
    calls.batch(
        "a plain reference to a target",
        vec![insert(
            &[b"near"],
            b"to a target",
            reference(&[b"near", b"value"]),
        )],
    );
    calls.get(&[b"near"], b"to a target");
    // A target written with a reference to it in one batch.
    let ops = vec![
        insert(
            &[b"sums"],
            b"value",
            Element::new_item_with_sum_item_allowing_bidirectional_references(b"paid".to_vec(), 9),
        ),
        insert(&[b"sums"], b"r3", bidirectional(beside(b"value"), true)),
    ];
    calls.batch("a target again with a new reference", ops);
    calls.get(&[], b"sums");
    calls.verify();

    // What a batch is refused for: a reference to a target that the batch
    // deletes, a target that is deleted while a reference does not
    // cascade, a reference in a partial batch, and a reference deeper
    // than the bound.
    calls.refused(|calls| {
        let ops = vec![
            insert(&[b"near"], b"r9", bidirectional(beside(b"second"), true)),
            delete(&[b"near"], b"second"),
        ];
        calls.batch("to a target the batch deletes", ops);
    });
    calls.batch(
        "a reference that does not cascade",
        vec![insert(
            &[b"near"],
            b"held",
            bidirectional(beside(b"second"), false),
        )],
    );
    calls.refused(|calls| calls.batch("its target deleted", vec![delete(&[b"near"], b"second")]));
    calls.refused(|calls| {
        let ops = vec![insert(
            &[b"near"],
            b"r8",
            bidirectional(beside(b"value"), true),
        )];
        calls.partial("a reference in a partial batch", ops, None, |_| {
            Ok(Vec::new())
        });
    });
    calls.refused(|calls| {
        let wrapped = Element::NonCounted(Box::new(target(b"wrapped")));
        calls.batch(
            "a target in a wrapper",
            vec![insert(&far[..2], b"wrapped", wrapped)],
        );
    });
    let mut deep: Vec<Vec<u8>> = vec![b"near".to_vec()];
    while deep.len() <= MAX_BACKWARD_REFERENCES_GROVE_DEPTH {
        let segment = format!("d{}", deep.len()).into_bytes();
        let above: Vec<&[u8]> = deep.iter().map(Vec::as_slice).collect();
        calls.insert(&above, &segment, Element::empty_tree());
        deep.push(segment);
    }
    let deepest: Vec<&[u8]> = deep.iter().map(Vec::as_slice).collect();
    calls.refused(|calls| {
        let ops = vec![insert(
            &deepest,
            b"ref",
            bidirectional(at(&[b"near", b"value"]), true),
        )];
        calls.batch("deeper than the bound", ops);
    });
    let ops = vec![insert(
        &deepest[..MAX_BACKWARD_REFERENCES_GROVE_DEPTH],
        b"ref",
        bidirectional(at(&[b"near", b"value"]), true),
    )];
    calls.batch("at the bound", ops);
    calls.batch(
        "the target once more",
        vec![insert(&[b"near"], b"value", target(b"seen from the bound"))],
    );
    calls.get(&deepest[..MAX_BACKWARD_REFERENCES_GROVE_DEPTH], b"ref");
    calls.verify();

    // Deletes: the plain reference, a reference alone, and a target with
    // every reference that cascades, the one three trees down and the one
    // at the bound too.
    calls.batch(
        "the plain reference deleted",
        vec![delete(&[b"near"], b"to a target")],
    );
    calls.batch("a reference deleted", vec![delete(&[b"near"], b"held")]);
    calls.batch("a target deleted", vec![delete(&[b"near"], b"second")]);
    calls.refused(|calls| calls.get(&[b"near"], b"r2"));
    calls.batch(
        "a target with references far away deleted",
        vec![delete(&[b"near"], b"value")],
    );
    calls.refused(|calls| calls.get(far, b"from afar"));
    calls.refused(|calls| calls.get(&[b"near"], b"r1"));
    calls.get(&[], b"far");
    // A target is written over by a plain item, and its chain goes.
    calls.batch(
        "a plain item over a target",
        vec![insert(&[b"sums"], b"value", Element::new_sum_item(1))],
    );
    calls.refused(|calls| calls.get(&[b"sums"], b"r2"));
    calls.get(&[], b"sums");
    calls.verify();

    drop(calls);
    let transaction = db.start_transaction();
    let mut calls = Calls::new(&db, version, Some(&transaction), &mut log);
    let ops = vec![
        insert(&[b"near"], b"value", target(b"in a transaction")),
        insert(
            far,
            b"from afar",
            bidirectional(at(&[b"near", b"value"]), true),
        ),
    ];
    calls.batch("a target and a reference in a transaction", ops);
    db.set_savepoint(&transaction).expect("a savepoint");
    calls.batch(
        "to be undone",
        vec![insert(&[b"near"], b"value", target(b"undone"))],
    );
    calls.get(far, b"from afar");
    db.rollback_to_savepoint(&transaction).expect("a rollback");
    calls.root();
    calls.get(far, b"from afar");
    calls.batch(
        "in its place",
        vec![insert(&[b"near"], b"value", target(b"kept"))],
    );
    calls.verify();
    drop(calls);
    let committed = db.commit_transaction(transaction);
    log.answer("commit".to_owned(), committed);
    let mut calls = Calls::new(&db, version, None, &mut log);
    calls.root();
    calls.get(far, b"from afar");
    calls.verify();
    db.flush().expect("a flush");
    log
}

/// On the grove versions whose batches maintain no backward references,
/// a batch is refused for an element that takes them and for a reference
/// that asks for one, and writes nothing.
fn family_refused<E: Engine>(dir: &Path, version: &'static GroveVersion) -> Log {
    let db = open::<E>(dir);
    let mut log = Log::default();
    let mut calls = Calls::new(&db, version, None, &mut log);
    let ops = vec![
        insert(&[], b"near", Element::empty_tree()),
        insert(&[b"near"], b"plain", Element::new_item(vec![1; 10])),
    ];
    calls.batch("trees", ops);
    calls.refused(|calls| {
        calls.batch(
            "a target",
            vec![insert(&[b"near"], b"value", target(b"hello"))],
        )
    });
    calls.refused(|calls| {
        let ops = vec![insert(
            &[b"near"],
            b"r1",
            bidirectional(beside(b"plain"), true),
        )];
        calls.batch("a reference", ops);
    });
    calls.refused(|calls| {
        let ops = vec![
            insert(&[b"near"], b"kept out", Element::new_item(vec![2; 20])),
            insert(&[b"near"], b"value", target(b"hello")),
        ];
        calls.partial("a target in a partial batch", ops, None, |_| Ok(Vec::new()));
    });
    calls.refused(|calls| calls.get(&[b"near"], b"kept out"));
    calls.get(&[], b"near");
    db.flush().expect("a flush");
    log
}

#[test]
fn backward_references_in_batches_are_refused_alike_on_grove_v1_to_v3() {
    for version in &GROVE_VERSIONS[..3] {
        on_both_engines!(family_refused, version);
    }
}

fn backward_references_in_batches_on(version: &'static GroveVersion) {
    let log = on_both_engines!(bidirectional_references, version);
    // The grove was verified after each stage of the history, references
    // and backward references with it. One issue was found, after the
    // fourth: the plain reference to a target, which no batch writes
    // again when its target is.
    let verified = log.answers.iter().filter(|(call, ..)| call == "verify");
    let verified: Vec<&str> = verified.map(|(.., issues)| issues.as_str()).collect();
    let stale = r#"Ok(["at/near/to a target"])"#;
    assert_eq!(
        verified,
        ["Ok([])", "Ok([])", "Ok([])", stale, "Ok([])", "Ok([])", "Ok([])"]
    );
}

#[test]
fn backward_references_in_batches_on_grove_v4() {
    backward_references_in_batches_on(&GROVE_VERSIONS[3]);
}

#[test]
fn backward_references_in_batches_on_grove_v5() {
    backward_references_in_batches_on(&GROVE_VERSIONS[4]);
}

/// How many trees deep the grove of `verified_deep` is, one inside the
/// other, and the stack its verification is given.
const DEEP: usize = 64;
const STACK_OF_THE_VERIFICATION: usize = 1024 * 1024;

/// A grove [`DEEP`] trees deep with an item in the innermost and one in
/// every eighth, made by a thread with a stack to spare. A second thread
/// with a stack of [`STACK_OF_THE_VERIFICATION`] bytes then opens the
/// database and verifies the grove. Gives how many issues it found.
fn verified_deep<E: Engine>(dir: &Path, version: &'static GroveVersion) -> usize {
    let on_a_stack_of = |bytes: usize, run: &(dyn Fn() -> usize + Sync)| {
        std::thread::scope(|scope| {
            let thread = std::thread::Builder::new().stack_size(bytes);
            let thread = thread.spawn_scoped(scope, run).expect("a thread");
            thread.join().expect("the thread ends")
        })
    };
    let made = on_a_stack_of(64 * 1024 * 1024, &|| {
        let db = open::<E>(dir);
        let mut at: Vec<Vec<u8>> = Vec::new();
        for level in 0..DEEP {
            let key = format!("level {level}").into_bytes();
            let inserted = db.insert(
                segments(&at).as_slice(),
                &key,
                Element::empty_tree(),
                None,
                None,
                version,
            );
            inserted.unwrap().expect("a tree");
            at.push(key);
            if level % 8 == 7 {
                let item = Element::new_item(vec![level as u8; 30]);
                let inserted =
                    db.insert(segments(&at).as_slice(), b"item", item, None, None, version);
                inserted.unwrap().expect("an item");
            }
        }
        db.flush().expect("a flush");
        at.len()
    });
    assert_eq!(made, DEEP);
    on_a_stack_of(STACK_OF_THE_VERIFICATION, &|| {
        let db = open::<E>(dir);
        let issues = db.verify_grovedb(None, true, true, version);
        issues.expect("the verification ends").len()
    })
}

/// `verify_grovedb` calls itself once for each level of the grove. What
/// one level keeps on the stack is small: the walk of a Merk's elements,
/// whose frame is large, has returned before the Merk of a tree in it is
/// walked. A grove [`DEEP`] trees deep is verified on a stack half
/// the size of a test thread's.
#[test]
fn the_verification_of_a_deep_grove_keeps_little_on_the_stack_for_each_level() {
    for version in [&GROVE_VERSIONS[0], &GROVE_VERSIONS[4]] {
        let dir = tempfile::tempdir().expect("a directory");
        assert_eq!(verified_deep::<LigninEngine>(dir.path(), version), 0);
        #[cfg(feature = "rocksdb_storage")]
        {
            let dir = tempfile::tempdir().expect("a directory");
            assert_eq!(
                verified_deep::<crate::RocksDbEngine>(dir.path(), version),
                0
            );
        }
    }
}

/// How many trees deep the grove of `wrong_hashes` is, the depths of the
/// trees whose Merk is written behind the back of its element, and those
/// of the references whose target is written again.
const CHAIN: usize = 12;
const PLANTED: [usize; 4] = [0, 3, 7, 11];
const STALE: [usize; 2] = [2, 9];

/// A grove with wrong hashes at several depths, and what `verify_grovedb`
/// reports of it: each issue as its path and its three hashes, in the
/// order of the paths.
///
/// The grove is a chain of [`CHAIN`] trees of three kinds in turn, one
/// inside the other, each with an element `m`. At the depths of
/// [`STALE`] a reference to `m` is written, and `m` again after it. At
/// the depths of [`PLANTED`] an element is written into the Merk of the
/// tree through the engine, and nothing is carried up: the element of
/// that tree commits to the root hash its Merk had before. Its key is
/// one that leaves the root of the Merk where it is, which the element
/// names.
///
/// `damaged` is told the database before the hashes are planted: an
/// engine that checks what it holds when it is closed is told there to
/// leave this one out.
fn wrong_hashes<E: Engine>(
    dir: &Path,
    version: &'static GroveVersion,
    damaged: impl FnOnce(&GroveDb<E>),
) -> Vec<String> {
    let db = open::<E>(dir);
    damaged(&db);
    let kind = |level: usize| &KINDS[level % 3];
    let mut at: Vec<Vec<u8>> = Vec::new();
    let mut trees = Vec::new();
    let put = |at: &[Vec<u8>], key: &[u8], element: Element| {
        let inserted = db.insert(segments(at).as_slice(), key, element, None, None, version);
        inserted.unwrap().expect("an element");
    };
    for level in 0..CHAIN {
        put(&at, b"t", (kind(level).make)());
        at.push(b"t".to_vec());
        put(&at, b"m", number(kind(level).sums, level as i64));
        trees.push(at.clone());
    }
    for level in STALE {
        let sibling = ReferencePathType::SiblingReference(b"m".to_vec());
        put(&trees[level], b"r", Element::new_reference(sibling));
        put(
            &trees[level],
            b"m",
            number(kind(level).sums, 100 + level as i64),
        );
    }
    for level in PLANTED {
        let transaction = db.start_transaction();
        let batch = E::Batch::default();
        {
            let at = segments(&trees[level]);
            let mut merk = db
                .open_transactional_merk_at_path(
                    at.as_slice().into(),
                    &transaction,
                    Some(&batch),
                    version,
                )
                .unwrap()
                .expect("the Merk of a tree");
            let planted = number(kind(level).sums, 1000);
            planted
                .insert(&mut merk, b"a", None, version)
                .unwrap()
                .expect("an element in it");
        }
        (db.db.apply_batch(batch, &transaction).unwrap()).expect("the batch is applied");
        db.commit_transaction(transaction)
            .unwrap()
            .expect("a commit");
    }
    let issues = db
        .verify_grovedb(None, true, true, version)
        .expect("the verification ends");
    let mut issues: Vec<(String, String)> = (issues.iter())
        .map(|(path, (root, expected, actual))| {
            let hashes = [root, expected, actual].map(hex::encode).join(" ");
            (named("at", &segments(path)), hashes)
        })
        .collect();
    issues.sort();
    let issues = issues.iter();
    issues
        .map(|(at, hashes)| format!("{at}: {hashes}"))
        .collect()
}

/// The digest of every issue that `verify_grovedb` reports of the grove of
/// `wrong_hashes`, each with its hashes. It is what the verification
/// reported of this grove before its walk was split in two functions.
const ISSUES_OF_THE_WRONG_HASHES: &str =
    "61322a7f39c88e18862f8614797cf4c69c6c00c9a9c79bb68130f8471bca925d";

/// `verify_grovedb` walks a grove with wrong hashes at several depths
/// and reports each of them, and nothing else, with the same hashes on
/// both engines, which are the hashes it reported before its walk was
/// split in two functions.
#[test]
fn the_verification_reports_the_wrong_hashes_of_a_grove_at_every_depth() {
    let under = |depth: usize, key: &str| format!("at{}{key}", "/t".repeat(depth + 1));
    let mut expected: Vec<String> = (PLANTED.iter().map(|depth| under(*depth, "")))
        .chain(STALE.iter().map(|depth| under(*depth, "/r")))
        .collect();
    expected.sort();
    for version in [&GROVE_VERSIONS[0], &GROVE_VERSIONS[4]] {
        let dir = tempfile::tempdir().expect("a directory");
        // A close of LigninDB in a debug build checks the hashes of every
        // Merk in use, and would find the planted ones.
        let unchecked = |db: &GroveDb<LigninEngine>| db.db.database().unchecked_at_close();
        let issues = wrong_hashes(dir.path(), version, unchecked);
        let at: Vec<&str> = (issues.iter())
            .map(|issue| issue.split(": ").next().expect("a path"))
            .collect();
        assert_eq!(at, expected);
        let digest = blake3::hash(issues.join("\n").as_bytes());
        eprintln!(
            "the issues of the grove with wrong hashes on grove version {}: {}",
            version.protocol_version,
            digest.to_hex()
        );
        assert_eq!(digest.to_hex().as_str(), ISSUES_OF_THE_WRONG_HASHES);
        #[cfg(feature = "rocksdb_storage")]
        {
            let dir = tempfile::tempdir().expect("a directory");
            let on_rocksdb = wrong_hashes::<crate::RocksDbEngine>(dir.path(), version, |_| ());
            assert_eq!(issues, on_rocksdb);
        }
    }
}
