//! The batch on a write transaction of a store on disk: what the
//! transaction reads back, what its write set holds, and what is committed
//! (`transactions.md` T-CF6).

use std::{collections::BTreeMap, sync::Arc};

use lignin_format::tree_page::Side;
use lignin_tree::{node::NodeHandle, value::ValueBuf};

use super::{items, leaf, subtree, tree, unit, Rng};
use crate::{
    batch::{Resolved, StagedBatch},
    snapshot::Versions,
    txn::{
        tests::store::{Db, Outcome},
        Applied, BatchError, Column, Hosted, NodePut, NodeView, Subtree, TxnError, Unit, ValuePut,
        WriteTxn,
    },
};

type Items = BTreeMap<Vec<u8>, Vec<u8>>;

/// A Merk that every store of the tests has entered, with no host.
const HOST: u8 = 1;
const OTHER: u8 = 2;
/// Subtrees that a test enters itself.
const CHILD: u8 = 210;
const BELOW: u8 = 211;

struct Store {
    db: Arc<Db>,
    versions: Versions,
}

impl Store {
    fn new() -> Self {
        let (db, versions) = Db::new(256);
        db.check_hashes_by(Box::new(super::TestRules));
        Self { db, versions }
    }

    fn begin(&self) -> WriteTxn<'_> {
        self.db.path(&self.versions).begin()
    }

    fn commit(&self, txn: &WriteTxn<'_>) -> Outcome {
        self.db.commit(&self.versions, txn).expect("a commit")
    }

    /// Applies `batch` in a transaction of its own and commits it.
    fn write<C>(&self, batch: StagedBatch<C>) {
        let txn = self.begin();
        apply(&txn, batch).expect("the batch is applied");
        assert!(matches!(self.commit(&txn), Outcome::Committed(_)));
    }

    /// The keys of every committed Merk that has a node, with the tree
    /// pages checked.
    fn keys(&self) -> BTreeMap<u8, Vec<String>> {
        let keys = self.db.check_trees(&self.versions, self.versions.latest());
        (keys.into_iter())
            .map(|(subtree, keys)| {
                let keys = keys
                    .iter()
                    .map(|key| String::from_utf8_lossy(key).into_owned());
                (subtree.as_bytes()[0], keys.collect())
            })
            .collect()
    }
}

fn apply<C>(txn: &WriteTxn<'_>, batch: StagedBatch<C>) -> Result<Applied, TxnError> {
    let scope = txn.scope()?;
    let applied = batch.apply(&scope);
    scope.merge();
    applied
}

fn apply_resolved<C>(txn: &WriteTxn<'_>, resolved: &Resolved<C>) -> Applied {
    let scope = txn.scope().expect("a scope");
    let applied = resolved.apply(&scope).expect("the batch is applied");
    scope.merge();
    applied
}

fn get(txn: &WriteTxn<'_>, unit: Unit, key: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let found = txn.get(unit, key, &mut out).expect("the record is read");
    found.then_some(out)
}

fn value_of(view: &NodeView<'_>) -> Vec<u8> {
    let mut buf = ValueBuf::new();
    view.value(&mut buf).expect("the value is read").to_vec()
}

fn node_value(txn: &WriteTxn<'_>, name: u8, key: &[u8]) -> Option<Vec<u8>> {
    txn.node(subtree(name), key, |view| view.as_ref().map(value_of))
        .expect("the node is read")
}

/// The handle of the node under `key` of the Merk `name` as `txn` shows
/// it: what a writer has from its read of a node.
fn handle_of(txn: &WriteTxn<'_>, name: u8, key: &[u8]) -> Option<NodeHandle> {
    txn.node(subtree(name), key, |view| {
        view.and_then(|view| view.handle())
    })
    .expect("the node is read")
}

/// Stages what makes the Merk `name` with the items `before` the Merk with
/// the items `after`, as a Merk that was open on `before` would: every
/// node whose record differs, with its value stated where the value is
/// new, a delete of every node that left, and the root.
///
/// No node names a read: for a Merk whose values the batch states, or
/// holds from an earlier operation.
fn stage_merk<C: Default>(batch: &StagedBatch<C>, name: u8, before: &Items, after: &Items) {
    stage_merk_read(batch, None, name, before, after);
}

/// [`stage_merk`], as a Merk that was open through `txn` would: a node
/// that keeps its value names the read the value is kept from, by the
/// handle `txn` shows the node with. A batch that keeps a committed value
/// and names no read is refused (`BatchError::NoRead`).
fn stage_merk_read<C: Default>(
    batch: &StagedBatch<C>,
    txn: Option<&WriteTxn<'_>>,
    name: u8,
    before: &Items,
    after: &Items,
) {
    let (_, was) = tree(before);
    let (root, now) = tree(after);
    if was == now {
        return;
    }
    for (key, node) in &now {
        let old = was.get(key);
        if old != Some(node) {
            let stated = old.is_none_or(|old| old.value != node.value);
            let mut put = node.put(stated, Hosted::Keep);
            if !stated {
                put.at = txn.and_then(|txn| handle_of(txn, name, key));
            }
            batch.put_node(subtree(name), &put, C::default());
        }
    }
    for key in was.keys().filter(|key| !now.contains_key(*key)) {
        batch.delete_node(subtree(name), key, C::default());
    }
    batch.set_root(subtree(name), root.as_deref());
}

/// Every part of every node of the Merk `name` under `keys`, as `txn`
/// shows it.
fn nodes(txn: &WriteTxn<'_>, name: u8, keys: &[Vec<u8>]) -> Vec<String> {
    (keys.iter())
        .filter_map(|key| {
            let shown = |view: Option<NodeView<'_>>| {
                view.map(|view| {
                    let link = |side| {
                        view.link(side)
                            .map(|link| (*link.hash, link.heights, link.aggregate.to_vec()))
                    };
                    format!(
                        "{:?} {:?} {:?} {:?} {:?} {:?} {:?}",
                        view.key(),
                        value_of(&view),
                        view.feature(),
                        view.value_hash(),
                        view.kv_hash(),
                        link(Side::Left),
                        link(Side::Right),
                    )
                })
            };
            txn.node(subtree(name), key, shown)
                .expect("the node is read")
        })
        .collect()
}

/// The kinds of record the rule of a batch is checked on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Flat(Column),
    Node,
}

#[derive(Debug, Clone)]
enum Call {
    Put(&'static [u8], u32),
    Delete(u32),
    Next,
    Merge(Vec<Call>),
    MergeOverwriting(Vec<Call>),
}

fn staged(kind: Kind, calls: &[Call]) -> StagedBatch<u32> {
    let batch = StagedBatch::new();
    for call in calls {
        match (call, kind) {
            (Call::Put(value, tag), Kind::Flat(column)) => {
                batch.put(unit(column, OTHER), b"k", value, *tag);
            }
            (Call::Delete(tag), Kind::Flat(column)) => {
                batch.delete(unit(column, OTHER), b"k", *tag);
            }
            (Call::Put(value, tag), Kind::Node) => {
                let node = leaf(b"k", value);
                batch.put_node(subtree(HOST), &node.put(true, Hosted::Keep), *tag);
                batch.set_root(subtree(HOST), Some(b"k"));
            }
            (Call::Delete(tag), Kind::Node) => {
                batch.delete_node(subtree(HOST), b"k", *tag);
                batch.set_root(subtree(HOST), None);
            }
            (Call::Next, _) => batch.next_operation(),
            (Call::Merge(other), _) => batch.merge(staged(kind, other)),
            (Call::MergeOverwriting(other), _) => batch.merge_overwriting(staged(kind, other)),
        }
    }
    batch
}

fn read(txn: &WriteTxn<'_>, kind: Kind, committed: bool) -> Option<Vec<u8>> {
    match (kind, committed) {
        (Kind::Flat(column), false) => get(txn, unit(column, OTHER), b"k"),
        (Kind::Flat(column), true) => {
            let mut out = Vec::new();
            let found = txn.committed_get(unit(column, OTHER), b"k", &mut out);
            found.expect("a read").then_some(out)
        }
        (Kind::Node, false) => node_value(txn, HOST, b"k"),
        (Kind::Node, true) => txn
            .committed_node(subtree(HOST), b"k", |view| view.as_ref().map(value_of))
            .expect("a read"),
    }
}

/// The calls on one key, the value of the survivor if it is a put, and what
/// the survivor attached.
type Case = (&'static str, Vec<Call>, Option<&'static [u8]>, u32);

/// T-CF6: in each case the transaction reads back the survivor, holds one
/// write-set entry for the key, and the survivor is the one outcome there
/// is to charge.
#[test]
fn the_transaction_reads_back_the_survivor_and_holds_one_entry_for_its_key() {
    use Call::{Delete, Merge, MergeOverwriting, Next, Put};
    let cases: [Case; 11] = [
        (
            "a put, then a delete",
            vec![Put(b"new", 1), Delete(2)],
            Some(b"new"),
            1,
        ),
        (
            "a delete, then a put",
            vec![Delete(1), Put(b"new", 2)],
            Some(b"new"),
            2,
        ),
        (
            "two puts",
            vec![Put(b"first", 1), Put(b"new", 2)],
            Some(b"new"),
            2,
        ),
        ("two deletes", vec![Delete(1), Delete(2)], None, 1),
        (
            "a later delete",
            vec![Put(b"new", 1), Next, Delete(2)],
            None,
            2,
        ),
        (
            "a later put",
            vec![Delete(1), Next, Put(b"new", 2)],
            Some(b"new"),
            2,
        ),
        (
            "a merged delete leaves the put of the generation",
            vec![Put(b"new", 1), Merge(vec![Delete(2)])],
            Some(b"new"),
            1,
        ),
        (
            "a merged delete leaves the delete of the generation",
            vec![Delete(1), Merge(vec![Delete(2)])],
            None,
            1,
        ),
        (
            "a merged put",
            vec![Delete(1), Merge(vec![Put(b"new", 2)])],
            Some(b"new"),
            2,
        ),
        (
            "a delete merged as a later operation",
            vec![Put(b"new", 1), MergeOverwriting(vec![Delete(2)])],
            None,
            2,
        ),
        (
            "a delete after a batch merged as a later operation",
            vec![MergeOverwriting(vec![Put(b"new", 1)]), Delete(2)],
            None,
            2,
        ),
    ];
    let kinds = [
        Kind::Flat(Column::Aux),
        Kind::Flat(Column::Roots),
        Kind::Flat(Column::Meta),
        Kind::Flat(Column::Data),
        Kind::Node,
    ];
    for kind in kinds {
        let store = Store::new();
        for (case, calls, survivor, tag) in &cases {
            let case = format!("{case}, {kind:?}");
            // The committed state holds a record under the key.
            store.write(staged(kind, &[Put(b"old", 0)]));

            let resolved = staged(kind, calls).resolve().expect("no call is refused");
            let outcomes: Vec<_> = resolved.outcomes().collect();
            assert_eq!(outcomes.len(), 1, "{case}: one outcome to charge");
            assert_eq!(outcomes[0].is_put(), survivor.is_some(), "{case}");
            assert_eq!(*outcomes[0].tag, *tag, "{case}: what the survivor attached");
            drop(outcomes);

            let txn = store.begin();
            apply_resolved(&txn, &resolved);
            assert_eq!(txn.stats().writes, 1, "{case}: one write-set entry");
            assert_eq!(read(&txn, kind, false).as_deref(), *survivor, "{case}");
            assert_eq!(
                read(&txn, kind, true).as_deref(),
                Some(&b"old"[..]),
                "{case}"
            );
            assert!(
                matches!(store.commit(&txn), Outcome::Committed(_)),
                "{case}"
            );
            let reader = store.begin();
            assert_eq!(read(&reader, kind, false).as_deref(), *survivor, "{case}");
            let nodes = store.keys();
            let expected = (kind == Kind::Node && survivor.is_some()).then(|| vec!["k".into()]);
            assert_eq!(nodes.get(&HOST), expected.as_ref(), "{case}");
        }
    }
}

#[test]
fn a_node_keeps_across_operations_the_value_one_of_them_wrote() {
    let store = Store::new();
    let first = items(&[(b"a", b"1"), (b"b", b"2")]);
    let second = items(&[(b"a", b"1"), (b"b", b"2"), (b"c", &[7; 600])]);
    let third = items(&[
        (b"a", b"one"),
        (b"b", b"2"),
        (b"c", &[7; 600]),
        (b"d", b"4"),
    ]);
    store.write({
        let batch = StagedBatch::<()>::new();
        stage_merk(&batch, HOST, &Items::new(), &first);
        batch
    });

    // The second operation writes `c`, with a value that is stored out of
    // line, and the third changes its links and leaves its value.
    let txn = store.begin();
    let batch = StagedBatch::<()>::new();
    stage_merk_read(&batch, Some(&txn), HOST, &first, &second);
    batch.next_operation();
    stage_merk_read(&batch, Some(&txn), HOST, &second, &third);
    let resolved = batch.resolve().expect("no call is refused");
    let kept = resolved.with_batch(|batch| {
        let stated = |node: &&NodePut<'_>| node.value != ValuePut::Keep;
        let stated = batch.merks[0].nodes.iter().filter(stated);
        stated.map(|node| node.key.to_vec()).collect::<Vec<_>>()
    });
    // `b` was written by both operations and its value by neither.
    assert_eq!(kept, [b"a".to_vec(), b"c".to_vec(), b"d".to_vec()]);

    apply_resolved(&txn, &resolved);
    assert_eq!(txn.stats().writes, 4);
    for (key, value) in &third {
        assert_eq!(node_value(&txn, HOST, key).as_ref(), Some(value));
    }
    assert!(matches!(store.commit(&txn), Outcome::Committed(_)));
    let reader = store.begin();
    for (key, value) in &third {
        assert_eq!(node_value(&reader, HOST, key).as_ref(), Some(value));
    }
    assert_eq!(store.keys()[&HOST], ["a", "b", "c", "d"]);
}

/// One edit of the model of the schedule below.
fn edit(rng: &mut Rng, merks: &mut [Items; 2], flat: &mut Items) {
    for merk in merks.iter_mut() {
        for _ in 0..rng.below(4) {
            let key = vec![b'a' + rng.below(9) as u8];
            match rng.below(3) {
                0 => {
                    merk.remove(&key);
                }
                _ => {
                    // Now and then a value that is stored out of line.
                    let len = match rng.below(6) {
                        0 => 300 + rng.below(300),
                        _ => 1 + rng.below(8),
                    };
                    merk.insert(key, vec![rng.below(250) as u8; len]);
                }
            }
        }
    }
    for _ in 0..rng.below(3) {
        let key = vec![b'0' + rng.below(4) as u8];
        match rng.below(3) {
            0 => {
                flat.remove(&key);
            }
            _ => {
                flat.insert(key, vec![rng.below(250) as u8; 1 + rng.below(5)]);
            }
        }
    }
}

/// Stages one operation: what takes the model from `before` to `after`,
/// with the nodes that keep their values read through `txn`.
fn stage_operation(
    batch: &StagedBatch,
    txn: Option<&WriteTxn<'_>>,
    before: &([Items; 2], Items),
    after: &([Items; 2], Items),
) {
    for (at, name) in [HOST, OTHER].into_iter().enumerate() {
        stage_merk_read(batch, txn, name, &before.0[at], &after.0[at]);
    }
    let aux = unit(Column::Aux, HOST);
    for (key, value) in &after.1 {
        if before.1.get(key) != Some(value) {
            batch.put(aux, key, value, ());
        }
    }
    for key in before.1.keys().filter(|key| !after.1.contains_key(*key)) {
        batch.delete(aux, key, ());
    }
}

fn dump(txn: &WriteTxn<'_>) -> Vec<String> {
    let keys: Vec<Vec<u8>> = (0..9).map(|at| vec![b'a' + at]).collect();
    let mut dump = nodes(txn, HOST, &keys);
    dump.extend(nodes(txn, OTHER, &keys));
    for at in 0..4 {
        let key = [b'0' + at];
        dump.push(format!("{:?}", get(txn, unit(Column::Aux, HOST), &key)));
    }
    dump
}

/// A batch whose operations are divided by `next_operation` gives the
/// transaction what the operations give it one after the other, each as a
/// batch of its own.
#[test]
fn one_batch_is_what_its_operations_are_one_after_the_other() {
    for seed in 1..=6u64 {
        let mut rng = Rng(seed.wrapping_mul(0x2545_f491_4f6c_dd1d) | 1);
        let (apart, together) = (Store::new(), Store::new());
        let mut state = ([Items::new(), Items::new()], Items::new());
        // What is committed before the operations.
        let empty = state.clone();
        for _ in 0..2 {
            edit(&mut rng, &mut state.0, &mut state.1);
        }
        for store in [&apart, &together] {
            let batch = StagedBatch::new();
            stage_operation(&batch, None, &empty, &state);
            if !batch.is_empty() {
                store.write(batch);
            }
        }

        let (one, other) = (apart.begin(), together.begin());
        let batch = StagedBatch::new();
        for _ in 0..2 + rng.below(5) {
            let before = state.clone();
            edit(&mut rng, &mut state.0, &mut state.1);
            let operation = StagedBatch::new();
            stage_operation(&operation, Some(&one), &before, &state);
            apply(&one, operation).expect("the operation is applied");
            batch.next_operation();
            stage_operation(&batch, Some(&other), &before, &state);
        }
        let resolved = batch.resolve().expect("no call is refused");
        apply_resolved(&other, &resolved);
        assert_eq!(
            other.stats().writes,
            resolved.len(),
            "seed {seed}: one entry for a key"
        );
        assert!(one.stats().writes >= resolved.len(), "seed {seed}");

        let expected: Vec<Vec<u8>> = (state.0.iter())
            .flat_map(|merk| merk.values().cloned())
            .collect();
        let shown = |txn: &WriteTxn<'_>| {
            let keys: Vec<Vec<u8>> = (0..9).map(|at| vec![b'a' + at]).collect();
            let values = |name| {
                keys.iter()
                    .filter_map(move |key| node_value(txn, name, key))
            };
            values(HOST).chain(values(OTHER)).collect::<Vec<_>>()
        };
        assert_eq!(shown(&other), expected, "seed {seed}");
        assert_eq!(dump(&one), dump(&other), "seed {seed}");
        for (store, txn) in [(&apart, &one), (&together, &other)] {
            let outcome = store.commit(txn);
            assert_eq!(
                matches!(outcome, Outcome::Empty),
                resolved.is_empty(),
                "seed {seed}"
            );
        }
        assert_eq!(dump(&apart.begin()), dump(&together.begin()), "seed {seed}");
        let keys = together.keys();
        assert_eq!(apart.keys(), keys, "seed {seed}");
        for (at, name) in [HOST, OTHER].into_iter().enumerate() {
            let expected: Vec<String> = (state.0[at].keys())
                .map(|key| String::from_utf8_lossy(key).into_owned())
                .collect();
            assert_eq!(
                keys.get(&name).cloned().unwrap_or_default(),
                expected,
                "seed {seed}"
            );
        }
    }
}

#[test]
fn a_merk_and_its_host_across_the_operations_of_one_batch() {
    let store = Store::new();
    // The element is written again with hashes that no rule makes, to tell
    // the two records of it apart.
    store.db.unhashed();
    let host = items(&[(b"h", b"element")]);
    let inner = items(&[(b"a", b"1"), (b"b", b"2"), (b"c", b"3")]);

    // One operation writes the element and enters the subtree it hosts, a
    // later one fills the Merk and writes the element's hashes again.
    let batch = StagedBatch::<()>::new();
    batch.host(subtree(HOST), b"h", &[subtree(CHILD)]);
    batch.enter(subtree(CHILD), b"merk");
    stage_merk(&batch, HOST, &Items::new(), &host);
    batch.next_operation();
    stage_merk(&batch, CHILD, &Items::new(), &inner);
    let mut again = leaf(b"h", b"unused");
    again.kv_hash = [5; 32];
    batch.put_node(subtree(HOST), &again.put(false, Hosted::Keep), ());
    let resolved = batch.resolve().expect("no call is refused");
    resolved.with_batch(|batch| {
        let named = Hosted::Set(&[subtree(CHILD)]);
        assert_eq!(batch.merks.len(), 2);
        assert_eq!(batch.merks[0].nodes[0].hosted, named);
        assert!(batch.merks.iter().all(|merk| !merk.anchored));
    });
    let txn = store.begin();
    let applied = apply_resolved(&txn, &resolved);

    // Where each node is comes back from the application, by its key.
    let placed: Vec<_> = resolved.placed(&applied).collect();
    let keys: Vec<(Subtree, &[u8])> = placed.iter().map(|(at, key, _)| (*at, *key)).collect();
    let child = subtree(CHILD);
    let expected: [(Subtree, &[u8]); 4] = [
        (subtree(HOST), b"h"),
        (child, b"a"),
        (child, b"b"),
        (child, b"c"),
    ];
    assert_eq!(keys, expected);
    let followed = txn.stats().handles_followed;
    for (subtree, key, handle) in placed {
        let found = |view: Option<NodeView<'_>>| view.map(|view| view.key().to_vec());
        let found = txn.follow(subtree, handle, key, found).expect("a read");
        assert_eq!(found.as_deref(), Some(key));
    }
    assert_eq!(
        txn.stats().handles_followed,
        followed + 4,
        "no handle had ended"
    );
    assert_eq!(
        node_value(&txn, HOST, b"h").as_deref(),
        Some(&b"element"[..])
    );
    assert!(matches!(store.commit(&txn), Outcome::Committed(_)));
    let keys = store.keys();
    assert_eq!(
        (&keys[&HOST], &keys[&CHILD]),
        (&vec!["h".into()], &vec!["a".into(), "b".into(), "c".into()])
    );

    // A batch that writes the Merk and not its host: the Merk is anchored.
    let more = items(&[(b"a", b"1"), (b"b", b"2"), (b"c", b"3"), (b"d", b"4")]);
    let batch = StagedBatch::<()>::new();
    batch.host(subtree(HOST), b"h", &[subtree(CHILD)]);
    let txn = store.begin();
    stage_merk_read(&batch, Some(&txn), CHILD, &inner, &more);
    let resolved = batch.resolve().expect("no call is refused");
    resolved.with_batch(|batch| assert!(batch.merks.len() == 1 && batch.merks[0].anchored));
    apply_resolved(&txn, &resolved);
    for (key, value) in &more {
        assert_eq!(node_value(&txn, CHILD, key).as_ref(), Some(value));
    }
    // Another node is the root of the Merk now, and the pointer in the
    // host is behind until the host is written (`tree-layout.md` T3): a
    // later batch writes it, as finalize does before a commit.
    let batch = StagedBatch::<()>::new();
    batch.host(subtree(HOST), b"h", &[subtree(CHILD)]);
    again.kv_hash = [6; 32];
    let mut put = again.put(false, Hosted::Keep);
    put.at = handle_of(&txn, HOST, b"h");
    batch.put_node(subtree(HOST), &put, ());
    apply(&txn, batch).expect("the host is written");
    assert!(matches!(store.commit(&txn), Outcome::Committed(_)));
    assert_eq!(store.keys()[&CHILD], ["a", "b", "c", "d"]);

    // One operation writes below, and a later one drops the subtree and
    // deletes the element: what was staged below is not applied.
    let batch = StagedBatch::<()>::new();
    batch.host(subtree(HOST), b"h", &[subtree(CHILD)]);
    let txn = store.begin();
    stage_merk_read(&batch, Some(&txn), CHILD, &more, &inner);
    batch.put(unit(Column::Aux, CHILD), b"x", b"y", ());
    batch.next_operation();
    batch.drop_subtree(subtree(CHILD));
    stage_merk(&batch, HOST, &host, &Items::new());
    apply(&txn, batch).expect("the batch is applied");
    assert_eq!(
        txn.stats().writes,
        2,
        "the drop and the delete of the element"
    );
    assert_eq!(node_value(&txn, HOST, b"h"), None);
    assert_eq!(node_value(&txn, CHILD, b"a"), None);
    assert_eq!(get(&txn, unit(Column::Aux, CHILD), b"x"), None);
    assert!(matches!(store.commit(&txn), Outcome::Committed(_)));
    assert_eq!(store.keys(), BTreeMap::new());
}

/// A tree is deleted, and then the tree it was in: the delete of a tree up
/// the grove while it is empty, each step an operation of one batch.
#[test]
fn a_tree_and_then_the_tree_above_it_are_deleted_by_the_operations_of_one_batch() {
    let grove = items(&[(b"a", b"tree"), (b"z", b"item")]);
    let outer = items(&[(b"h", b"tree")]);
    let inner = items(&[(b"x", b"1"), (b"y", b"2")]);
    let stores = [Store::new(), Store::new()];
    for store in &stores {
        let batch = StagedBatch::<()>::new();
        batch.host(subtree(HOST), b"a", &[subtree(CHILD)]);
        batch.host(subtree(CHILD), b"h", &[subtree(BELOW)]);
        batch.enter(subtree(CHILD), b"merk");
        batch.enter(subtree(BELOW), b"merk");
        stage_merk(&batch, HOST, &Items::new(), &grove);
        stage_merk(&batch, CHILD, &Items::new(), &outer);
        stage_merk(&batch, BELOW, &Items::new(), &inner);
        batch.put(unit(Column::Aux, BELOW), b"x", b"old", ());
        store.write(batch);
    }
    // The operations: the inner tree goes with its element, then the outer
    // one with its own.
    let first = |batch: &StagedBatch, _: &WriteTxn<'_>| {
        // The handles the operation opens say where their subtrees hang.
        batch.host(subtree(HOST), b"a", &[subtree(CHILD)]);
        batch.host(subtree(CHILD), b"h", &[subtree(BELOW)]);
        batch.drop_subtree(subtree(BELOW));
        stage_merk(batch, CHILD, &outer, &Items::new());
    };
    let second = |batch: &StagedBatch, txn: &WriteTxn<'_>| {
        batch.drop_subtree(subtree(CHILD));
        let left = items(&[(b"z", b"item")]);
        stage_merk_read(batch, Some(txn), HOST, &grove, &left);
    };

    let [apart, together] = &stores;
    let one = apart.begin();
    for operation in [&first as &dyn Fn(&StagedBatch, &WriteTxn<'_>), &second] {
        let batch = StagedBatch::new();
        operation(&batch, &one);
        apply(&one, batch).expect("the operation is applied");
    }
    let other = together.begin();
    let batch = StagedBatch::new();
    first(&batch, &other);
    batch.next_operation();
    second(&batch, &other);
    let resolved = batch.resolve().expect("no call is refused");
    resolved.with_batch(|batch| assert_eq!(batch.subtrees.len(), 1, "one drop is named"));
    apply_resolved(&other, &resolved);

    let seen = |txn: &WriteTxn<'_>| {
        let keys: Vec<Vec<u8>> = [b"a", b"h", b"x", b"y", b"z"]
            .map(|key| key.to_vec())
            .into();
        let mut seen: Vec<String> = [HOST, CHILD, BELOW]
            .into_iter()
            .flat_map(|name| nodes(txn, name, &keys))
            .collect();
        seen.push(format!("{:?}", get(txn, unit(Column::Aux, BELOW), b"x")));
        seen
    };
    assert_eq!(seen(&one), seen(&other));
    assert_eq!(
        seen(&other).len(),
        2,
        "the item of the grove, and no record below"
    );
    for (store, txn) in [(apart, &one), (together, &other)] {
        assert!(matches!(store.commit(txn), Outcome::Committed(_)));
        assert_eq!(store.keys(), BTreeMap::from([(HOST, vec!["z".to_owned()])]));
    }
}

#[test]
fn a_subtree_that_one_operation_enters_and_a_later_one_drops_is_never_seen() {
    let store = Store::new();
    let batch = StagedBatch::<()>::new();
    batch.host(subtree(HOST), b"h", &[subtree(CHILD)]);
    batch.enter(subtree(CHILD), b"merk");
    stage_merk(&batch, HOST, &Items::new(), &items(&[(b"h", b"tree")]));
    stage_merk(&batch, CHILD, &Items::new(), &items(&[(b"a", b"1")]));
    batch.host(subtree(CHILD), b"a", &[subtree(BELOW)]);
    batch.enter(subtree(BELOW), b"flat");
    batch.put(unit(Column::Data, BELOW), b"entry", b"1", ());
    batch.put(unit(Column::Meta, BELOW), b"m", b"1", ());
    batch.next_operation();
    // The element is written again as one that hosts nothing.
    batch.drop_subtree(subtree(CHILD));
    let item = leaf(b"h", b"item");
    batch.put_node(subtree(HOST), &item.put(true, Hosted::Set(&[])), ());
    let resolved = batch.resolve().expect("no call is refused");
    resolved.with_batch(|batch| {
        assert!(batch.subtrees.is_empty() && batch.records.is_empty());
        assert_eq!(batch.merks.len(), 1);
    });

    let txn = store.begin();
    apply_resolved(&txn, &resolved);
    assert_eq!(txn.stats().writes, 1);
    assert_eq!(node_value(&txn, HOST, b"h").as_deref(), Some(&b"item"[..]));
    assert!(matches!(store.commit(&txn), Outcome::Committed(_)));
    assert_eq!(store.keys(), BTreeMap::from([(HOST, vec!["h".to_owned()])]));

    // Entered and dropped by one operation, it is the subtree of before
    // that is dropped, and there is none.
    let batch = StagedBatch::<()>::new();
    batch.enter(subtree(CHILD), b"merk");
    batch.drop_subtree(subtree(CHILD));
    let txn = store.begin();
    let refused = BatchError::NotEntered {
        subtree: subtree(CHILD),
    };
    assert!(matches!(apply(&txn, batch), Err(TxnError::Batch(error)) if error == refused));
    assert_eq!(txn.stats().writes, 0);
}

#[test]
fn a_drop_takes_what_is_staged_below_only_where_the_batch_was_told_of_it() {
    let store = Store::new();
    let host = items(&[(b"h", b"element")]);
    store.write({
        let batch = StagedBatch::<()>::new();
        batch.host(subtree(HOST), b"h", &[subtree(CHILD)]);
        batch.enter(subtree(CHILD), b"flat");
        stage_merk(&batch, HOST, &Items::new(), &host);
        batch.put(unit(Column::Data, CHILD), b"entry", b"old", ());
        batch.put(unit(Column::Aux, CHILD), b"x", b"old", ());
        batch
    });

    let operations = |told: bool| {
        let batch = StagedBatch::<()>::new();
        if told {
            batch.host(subtree(HOST), b"h", &[subtree(CHILD)]);
        }
        batch.put(unit(Column::Aux, CHILD), b"x", b"new", ());
        batch.next_operation();
        batch.drop_subtree(subtree(HOST));
        batch
    };
    // The write below stays in a batch that does not know it is below, and
    // the transaction refuses the batch whole.
    let txn = store.begin();
    let refused = BatchError::NotEntered {
        subtree: subtree(CHILD),
    };
    assert!(
        matches!(apply(&txn, operations(false)), Err(TxnError::Batch(error)) if error == refused)
    );
    assert_eq!(txn.stats().writes, 0);
    assert_eq!(
        get(&txn, unit(Column::Aux, CHILD), b"x").as_deref(),
        Some(&b"old"[..])
    );

    apply(&txn, operations(true)).expect("the batch is applied");
    assert_eq!(txn.stats().writes, 1, "the drop");
    assert_eq!(get(&txn, unit(Column::Aux, CHILD), b"x"), None);
    assert_eq!(get(&txn, unit(Column::Data, CHILD), b"entry"), None);
    assert_eq!(node_value(&txn, HOST, b"h"), None);
    assert!(matches!(store.commit(&txn), Outcome::Committed(_)));
    assert_eq!(store.keys(), BTreeMap::new());
}

#[test]
fn a_batch_that_is_refused_applies_nothing() {
    let store = Store::new();
    let batch = StagedBatch::<()>::new();
    batch.put(unit(Column::Aux, HOST), b"k", b"v", ());
    batch.clear(subtree(OTHER));
    batch.clear(subtree(OTHER));
    let txn = store.begin();
    let refused = BatchError::SubtreeTwice {
        subtree: subtree(OTHER),
    };
    assert!(matches!(apply(&txn, batch), Err(TxnError::Batch(error)) if error == refused));
    assert_eq!(txn.stats().writes, 0);
    assert_eq!(get(&txn, unit(Column::Aux, HOST), b"k"), None);
    assert!(matches!(store.commit(&txn), Outcome::Empty));

    // A batch that holds nothing adds nothing.
    let batch = StagedBatch::<()>::new();
    batch.set_root(subtree(HOST), Some(b"k"));
    batch.host(subtree(HOST), b"k", &[subtree(CHILD)]);
    let txn = store.begin();
    assert_eq!(
        apply(&txn, batch).expect("it is applied"),
        Applied::default()
    );
    assert!(matches!(store.commit(&txn), Outcome::Empty));
}

/// A batch is one operation of the write set, whatever it holds: a
/// rollback to a savepoint from before it takes all of it back.
#[test]
fn a_savepoint_before_a_batch_takes_the_whole_batch_back() {
    let store = Store::new();
    let host = items(&[(b"h", b"element")]);
    let inner = items(&[(b"a", b"1"), (b"b", b"2")]);
    let other = items(&[(b"x", b"1"), (b"y", b"2")]);
    store.write({
        let batch = StagedBatch::<()>::new();
        batch.host(subtree(HOST), b"h", &[subtree(CHILD)]);
        batch.enter(subtree(CHILD), b"merk");
        stage_merk(&batch, HOST, &Items::new(), &host);
        stage_merk(&batch, CHILD, &Items::new(), &inner);
        stage_merk(&batch, OTHER, &Items::new(), &other);
        batch.put(unit(Column::Aux, CHILD), b"x", b"old", ());
        batch.put(unit(Column::Meta, HOST), b"m", b"old", ());
        batch
    });
    let committed = store.keys();

    let txn = store.begin();
    let before = StagedBatch::<()>::new();
    before.put(unit(Column::Aux, HOST), b"before", b"stays", ());
    apply(&txn, before).expect("the batch is applied");
    let seen = |txn: &WriteTxn<'_>| {
        let keys: Vec<Vec<u8>> = [b"a", b"b", b"h", b"n", b"x", b"y"]
            .map(|key| key.to_vec())
            .into();
        let mut seen: Vec<String> = [HOST, CHILD, OTHER, BELOW]
            .into_iter()
            .flat_map(|name| nodes(txn, name, &keys))
            .collect();
        for (column, name, key) in [
            (Column::Aux, HOST, &b"before"[..]),
            (Column::Aux, CHILD, b"x"),
            (Column::Meta, HOST, b"m"),
            (Column::Data, BELOW, b"entry"),
            (Column::Roots, OTHER, b"r"),
        ] {
            seen.push(format!("{:?}", get(txn, unit(column, name), key)));
        }
        seen
    };
    let at_savepoint = seen(&txn);
    txn.set_savepoint().expect("a savepoint");

    // One batch of two operations: it drops a subtree and writes its host
    // without it, enters another below the host, clears a Merk and writes
    // it again, and puts and deletes flat records.
    let batch = StagedBatch::<()>::new();
    batch.drop_subtree(subtree(CHILD));
    batch.host(subtree(HOST), b"h", &[subtree(BELOW)]);
    batch.enter(subtree(BELOW), b"flat");
    let element = leaf(b"h", b"another element");
    batch.put_node(subtree(HOST), &element.put(true, Hosted::Keep), ());
    batch.put(unit(Column::Data, BELOW), b"entry", b"1", ());
    batch.next_operation();
    batch.clear(subtree(OTHER));
    stage_merk(&batch, OTHER, &Items::new(), &items(&[(b"n", b"new")]));
    batch.put(unit(Column::Roots, OTHER), b"r", b"n", ());
    batch.delete(unit(Column::Meta, HOST), b"m", ());
    let resolved = batch.resolve().expect("no call is refused");
    resolved.with_batch(|batch| {
        assert_eq!(
            (batch.subtrees.len(), batch.merks.len(), batch.records.len()),
            (3, 2, 3)
        );
    });
    apply_resolved(&txn, &resolved);
    assert_eq!(txn.stats().writes, 1 + 2 + resolved.len());
    assert_eq!(node_value(&txn, CHILD, b"a"), None);
    assert_eq!(get(&txn, unit(Column::Aux, CHILD), b"x"), None);
    assert_eq!(node_value(&txn, OTHER, b"x"), None);
    assert_eq!(node_value(&txn, OTHER, b"n").as_deref(), Some(&b"new"[..]));
    assert_eq!(
        get(&txn, unit(Column::Data, BELOW), b"entry").as_deref(),
        Some(&b"1"[..])
    );
    assert_eq!(get(&txn, unit(Column::Meta, HOST), b"m"), None);
    assert_ne!(seen(&txn), at_savepoint);

    txn.rollback_to_savepoint().expect("the rollback");
    assert_eq!(seen(&txn), at_savepoint);
    assert_eq!(txn.stats().writes, 1, "the write from before the savepoint");
    // The subtree the batch entered is entered no more.
    let into = StagedBatch::<()>::new();
    into.put(unit(Column::Data, BELOW), b"entry", b"2", ());
    let refused = BatchError::NotEntered {
        subtree: subtree(BELOW),
    };
    assert!(matches!(apply(&txn, into), Err(TxnError::Batch(error)) if error == refused));

    assert!(matches!(store.commit(&txn), Outcome::Committed(_)));
    assert_eq!(store.keys(), committed);
    let reader = store.begin();
    assert_eq!(seen(&reader), at_savepoint);
}
