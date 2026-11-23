//! Nodes and flat records in the data column of one subtree, where the
//! subtree is of a tree type that keeps its data as flat records
//! ([`Scope::apply_by_kind`](crate::txn::Scope::apply_by_kind),
//! `transactions.md` ST1).
//!
//! The model knows one kind for a data column: the tests tell it the
//! nodes they write, from which it makes the change sets, and ask it for
//! nothing.

use super::{
    model::{self, OwnedBatch, OwnedSubtree},
    store::{Db, Outcome, ENTERED, TREE_TYPE},
    subtrees::{aux, enter, planted, subtrees, INNER, OUTER},
    *,
};
use crate::txn::FlatKinds;

/// The tree type that keeps flat records in these tests.
const FLAT_TYPE: &[u8] = b"flat";

/// A subtree that is stated anew as one of that type before each test,
/// and one that stays of the type of a Merk.
const FLAT: u8 = 7;
const MERK: u8 = 8;

/// A subtree that no test enters.
const NEVER: u8 = ENTERED + 50;

fn kinds() -> FlatKinds {
    |tree_type| tree_type == FLAT_TYPE
}

fn committed(outcome: Outcome) -> bool {
    matches!(outcome, Outcome::Committed(_))
}

fn refusal(result: Result<Applied, TxnError>) -> BatchError {
    match result {
        Err(TxnError::Batch(error)) => error,
        other => panic!("a refused batch is expected, and not {other:?}"),
    }
}

fn not_flat(name: u8) -> BatchError {
    BatchError::NotFlat { unit: data(name) }
}

fn not_merk(name: u8) -> BatchError {
    BatchError::NotMerk {
        subtree: model::subtree(name),
    }
}

fn data(name: u8) -> Unit {
    model::unit(Column::Data, name)
}

/// States the subtree `name` anew as one of the tree type `tree_type`,
/// with no transaction.
fn restate<'s>(world: &mut World<'s>, name: u8, tree_type: &[u8], others: &[&Tx<'s>]) {
    let anew = OwnedSubtree::Convert(model::subtree(name), tree_type.to_vec(), None);
    assert!(committed(
        world.write_outside(&subtrees(vec![anew]), others)
    ));
}

/// A store in which the subtree `FLAT` is of the type that keeps flat
/// records.
fn store<'s>(db: &'s Db, versions: &'s Versions) -> World<'s> {
    let mut world = World::new(db, versions);
    restate(&mut world, FLAT, FLAT_TYPE, &[]);
    world
}

/// Applies `batch` through `scope`, which stays open: with the tree
/// types that keep flat records where `told` says so, and as any batch
/// otherwise. The model is told nothing.
fn stated(scope: &Scope<'_, '_>, batch: &OwnedBatch, told: bool) -> Result<Applied, TxnError> {
    let read = batch.through(scope);
    read.with(|batch| match told {
        true => scope.apply_by_kind(batch, kinds()),
        false => scope.apply(batch),
    })
}

/// Applies `batch` through a scope of `tx` that merges ([`stated`]).
fn apply<'s>(
    world: &World<'s>,
    tx: &Tx<'s>,
    batch: &OwnedBatch,
    told: bool,
) -> Result<Applied, TxnError> {
    let scope = tx.txn.scope()?;
    let first_write = world.latest().get();
    let applied = stated(&scope, batch, told);
    scope.merge();
    if applied.is_ok() {
        // The model is told the nodes, which it makes the next change set
        // of the Merk from, and not the flat records beside them.
        let nodes = OwnedBatch {
            records: Vec::new(),
            ..batch.clone()
        };
        world.note(tx, &nodes, first_write);
    }
    applied
}

/// A batch that puts the flat record `key` into the data column of `name`.
fn record(name: u8, key: &[u8], value: &[u8]) -> OwnedBatch {
    OwnedBatch {
        records: vec![put(data(name), key, value)],
        ..OwnedBatch::default()
    }
}

/// A batch that puts the node `key` into the Merk of `name`, as `tx` reads
/// the Merk.
fn node<'s>(world: &World<'s>, tx: &Tx<'s>, name: u8, key: &[u8]) -> OwnedBatch {
    OwnedBatch {
        merks: vec![world.merk(tx, name, |items| {
            items.insert(key.to_vec(), vec![3; 20]);
        })],
        ..OwnedBatch::default()
    }
}

/// What a read by kind shows under `key` of `name`: the bytes of a flat
/// record, that a node is there, or nothing.
fn shown(tx: &Tx<'_>, name: u8, key: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let shown = (tx.txn).node_by_kind(model::subtree(name), key, None, kinds(), |view| {
        view.map(|view| view.flat_record().map(<[u8]>::to_vec))
    });
    match shown {
        Ok(Some(Some(bytes))) => Ok(Some(bytes)),
        Ok(Some(None)) => Err("a node".to_owned()),
        Ok(None) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn a_node() -> Result<Option<Vec<u8>>, String> {
    Err("a node".to_owned())
}

/// The flat record under `key` of the data column of `name`, as a flat
/// read shows it.
fn flat(tx: &Tx<'_>, name: u8, key: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let mut out = Vec::new();
    let found = tx.txn.get(data(name), key, &mut out);
    found
        .map(|found| found.then_some(out))
        .map_err(|error| error.to_string())
}

/// Commits `tx` beside the model, which is told nothing of it.
fn commit_untold(db: &Db, versions: &Versions, tx: Tx<'_>) {
    let committer = db.committer(versions);
    let done = db.commit_as(&committer, &tx.txn, None).map(drop);
    done.expect("the transaction commits");
    tx.forget();
}

/// The refusal of a commit of `tx`, which is rolled back.
fn commit_refused(db: &Db, versions: &Versions, tx: Tx<'_>) -> TxnError {
    let committer = db.committer(versions);
    let refused = db.commit_as(&committer, &tx.txn, None).map(drop);
    let refused = refused.expect_err("the write set is not laid out");
    tx.txn.rollback().expect("no scope is open");
    tx.forget();
    match refused {
        crate::commit::CommitError::Refused(error) => error,
        other => panic!("a refused commit is expected, and not {other:?}"),
    }
}

/// A batch that is given the tree types that keep flat records puts a
/// flat record into the data column of a subtree of such a tree whose
/// Merk has nodes, and a node where the transaction wrote flat records
/// there: with the nodes committed before, with both written by one
/// transaction in either order, and after the commit.
#[test]
fn a_tree_that_keeps_flat_records_takes_nodes_and_flat_records() {
    let (db, versions) = Db::new(128);
    let mut world = store(&db, &versions);
    // The nodes are committed, and the record is put beside them.
    let tx = world.begin();
    apply(&world, &tx, &node(&world, &tx, FLAT, b"n1"), true).expect("a node");
    assert!(committed(world.commit(tx, &[])));
    let tx = world.begin();
    apply(&world, &tx, &record(FLAT, b"r1", b"flat bytes"), true)
        .expect("a flat record beside the nodes");
    assert_eq!(shown(&tx, FLAT, b"r1"), Ok(Some(b"flat bytes".to_vec())));
    assert_eq!(shown(&tx, FLAT, b"n1"), a_node());
    // A node after the transaction's own flat record.
    apply(&world, &tx, &node(&world, &tx, FLAT, b"n2"), true)
        .expect("a node beside the flat record");
    assert_eq!(shown(&tx, FLAT, b"n2"), a_node());
    assert_eq!(shown(&tx, FLAT, b"r1"), Ok(Some(b"flat bytes".to_vec())));
    // The record is deleted beside the nodes, and put again.
    let gone = OwnedBatch {
        records: vec![delete(data(FLAT), b"r1")],
        ..OwnedBatch::default()
    };
    apply(&world, &tx, &gone, true).expect("a delete of the flat record");
    assert_eq!(shown(&tx, FLAT, b"r1"), Ok(None));
    apply(&world, &tx, &record(FLAT, b"r1", b"flat bytes"), true).expect("put again");
    assert!(committed(world.commit(tx, &[])));
    let tx = world.begin();
    assert_eq!(shown(&tx, FLAT, b"r1"), Ok(Some(b"flat bytes".to_vec())));
    assert_eq!(shown(&tx, FLAT, b"n1"), a_node());
    assert_eq!(shown(&tx, FLAT, b"n2"), a_node());
    assert_eq!(shown(&tx, FLAT, b"none"), Ok(None));
    drop(tx);

    // The other order, in one transaction: the record while the Merk has
    // no node, and then its first node.
    let (db, versions) = Db::new(128);
    let mut world = store(&db, &versions);
    let tx = world.begin();
    apply(&world, &tx, &record(FLAT, b"r1", b"first"), true).expect("a flat record");
    apply(&world, &tx, &node(&world, &tx, FLAT, b"n1"), true)
        .expect("the first node, beside the transaction's flat record");
    apply(&world, &tx, &record(FLAT, b"r2", b"second"), true)
        .expect("a flat record beside the transaction's node");
    assert_eq!(shown(&tx, FLAT, b"r1"), Ok(Some(b"first".to_vec())));
    assert_eq!(shown(&tx, FLAT, b"r2"), Ok(Some(b"second".to_vec())));
    assert_eq!(shown(&tx, FLAT, b"n1"), a_node());
    assert!(committed(world.commit(tx, &[])));
    let tx = world.begin();
    assert_eq!(shown(&tx, FLAT, b"r2"), Ok(Some(b"second".to_vec())));
    assert_eq!(shown(&tx, FLAT, b"n1"), a_node());
}

/// A key holds one record. Under the key of a node of the Merk a flat
/// record is neither put nor deleted: of a node that is committed, of
/// one the transaction wrote, and of one it wrote that no link reaches.
/// A range delete is not taken where the Merk has nodes. A flat record
/// goes under the key of a node that the transaction removed. A node
/// that is put under the key of a flat record takes the record, as the
/// write of a node takes an unlinked record under its key: a flat read
/// of the key is refused by the transaction that wrote the node, and
/// finds nothing after its commit.
#[test]
fn a_key_holds_one_record() {
    let (db, versions) = Db::new(128);
    let mut world = store(&db, &versions);
    let under_node = BatchError::RecordUnderNode { unit: data(FLAT) };
    assert!(!under_node.is_conflict());
    let deleted = |key: &[u8]| OwnedBatch {
        records: vec![delete(data(FLAT), key)],
        ..OwnedBatch::default()
    };
    let tx = world.begin();
    apply(&world, &tx, &node(&world, &tx, FLAT, b"same"), true).expect("a node");
    apply(&world, &tx, &node(&world, &tx, FLAT, b"gone"), true).expect("a node");
    apply(&world, &tx, &record(FLAT, b"other", b"a record"), true).expect("a flat record");
    // Under the key of a node that the transaction wrote.
    let refused = apply(&world, &tx, &record(FLAT, b"same", b"a record"), true);
    assert_eq!(refusal(refused), under_node);
    assert_eq!(
        refusal(apply(&world, &tx, &deleted(b"same"), true)),
        under_node
    );
    assert_eq!(shown(&tx, FLAT, b"same"), a_node());
    assert!(committed(world.commit(tx, &[])));

    // Under the key of a committed node.
    let tx = world.begin();
    let refused = apply(&world, &tx, &record(FLAT, b"same", b"a record"), true);
    assert_eq!(refusal(refused), under_node);
    assert_eq!(
        refusal(apply(&world, &tx, &deleted(b"same"), true)),
        under_node
    );
    assert_eq!(shown(&tx, FLAT, b"same"), a_node());
    // A range delete, which is not taken where the Merk has nodes.
    let range = OwnedBatch {
        records: vec![Write::Range(data(FLAT), b"a".to_vec(), None)],
        ..OwnedBatch::default()
    };
    assert_eq!(refusal(apply(&world, &tx, &range, true)), not_flat(FLAT));
    assert_eq!(shown(&tx, FLAT, b"other"), Ok(Some(b"a record".to_vec())));
    // Under the key of a node that the transaction removed.
    let removed = OwnedBatch {
        merks: vec![world.merk(&tx, FLAT, |items| {
            items.remove(&b"gone"[..]);
        })],
        ..OwnedBatch::default()
    };
    apply(&world, &tx, &removed, true).expect("the node is removed");
    apply(&world, &tx, &record(FLAT, b"gone", b"in its place"), true)
        .expect("a flat record where the node was");
    assert_eq!(
        shown(&tx, FLAT, b"gone"),
        Ok(Some(b"in its place".to_vec()))
    );

    // The other order: the record is committed, and the node takes it.
    apply(&world, &tx, &node(&world, &tx, FLAT, b"other"), true)
        .expect("a node under the key of the record");
    assert_eq!(shown(&tx, FLAT, b"other"), a_node());
    // The transaction has its own node under the key, and a flat read
    // of the key is refused.
    let refused = flat(&tx, FLAT, b"other").expect_err("a node of the transaction");
    assert!(
        refused.contains("a Merk node is read as a flat record"),
        "{refused}"
    );
    assert!(committed(world.commit(tx, &[])));
    let tx = world.begin();
    assert_eq!(shown(&tx, FLAT, b"other"), a_node());
    assert_eq!(
        flat(&tx, FLAT, b"other"),
        Ok(None),
        "the node took the record"
    );
    assert_eq!(flat(&tx, FLAT, b"gone"), Ok(Some(b"in its place".to_vec())));
    drop(tx);

    // Under the key of a node that the transaction wrote and no link
    // reaches: its change set states no root for a Merk that has none.
    const LOOSE: u8 = 9;
    restate(&mut world, LOOSE, FLAT_TYPE, &[]);
    let tx = world.begin();
    let mut loose = node(&world, &tx, LOOSE, b"loose");
    loose.merks[0].root = None;
    let scope = tx.txn.scope().expect("a scope");
    stated(&scope, &loose, true).expect("a node that no link reaches");
    let refused = stated(&scope, &record(LOOSE, b"loose", b"a record"), true);
    assert_eq!(
        refusal(refused),
        BatchError::RecordUnderNode { unit: data(LOOSE) }
    );
    stated(&scope, &record(LOOSE, b"beside", b"a record"), true)
        .expect("a flat record under another key");
    // A batch that clears the subtree takes that node with it: the key
    // is free for a flat record of the same batch, beside its first node.
    let mut batch = node(&world, &tx, LOOSE, b"a");
    batch.records.push(put(data(LOOSE), b"loose", b"a record"));
    let cleared = OwnedSubtree::Clear(model::subtree(LOOSE));
    batch.subtrees.push(cleared);
    stated(&scope, &batch, true).expect("the key of a node that went with the clear");
    scope.rollback();
    tx.txn.rollback().expect("no scope is open");
    tx.forget();
}

/// Under every other tree type the two are refused as they were, also
/// for a batch that is given the tree types that keep flat records, and
/// a batch that is given none is refused under a tree that keeps them.
#[test]
fn under_another_tree_type_and_for_a_batch_that_is_told_nothing_the_two_are_refused() {
    let (db, versions) = Db::new(128);
    let mut world = store(&db, &versions);
    let tx = world.begin();
    apply(&world, &tx, &node(&world, &tx, MERK, b"n1"), true).expect("a node");
    apply(&world, &tx, &node(&world, &tx, FLAT, b"n1"), true).expect("a node");
    assert!(committed(world.commit(tx, &[])));

    // A flat record where the Merk has nodes.
    let tx = world.begin();
    let refused = apply(&world, &tx, &record(MERK, b"r", b"x"), true);
    assert_eq!(refusal(refused), not_flat(MERK));
    let refused = apply(&world, &tx, &record(FLAT, b"r", b"x"), false);
    assert_eq!(refusal(refused), not_flat(FLAT));
    assert_eq!(shown(&tx, FLAT, b"r"), Ok(None));
    // A node where the transaction wrote flat records: a subtree of a
    // Merk's type whose Merk has no node takes the record, and not the
    // node after it.
    const BARE: u8 = 9;
    apply(&world, &tx, &record(BARE, b"r", b"x"), true).expect("a flat record, and no node");
    let refused = apply(&world, &tx, &node(&world, &tx, BARE, b"n"), true);
    assert_eq!(refusal(refused), not_merk(BARE));
    // And a flat record where the transaction wrote a node that its
    // change set then removed: the Merk has no root again.
    const EMPTIED: u8 = 11;
    apply(&world, &tx, &node(&world, &tx, EMPTIED, b"n"), true).expect("a node");
    let removed = OwnedBatch {
        merks: vec![world.merk(&tx, EMPTIED, |items| {
            items.remove(&b"n"[..]);
        })],
        ..OwnedBatch::default()
    };
    apply(&world, &tx, &removed, true).expect("the node is removed");
    let refused = apply(&world, &tx, &record(EMPTIED, b"r", b"x"), true);
    assert_eq!(refusal(refused), not_flat(EMPTIED));
    drop(tx);
    // And under the tree that keeps flat records, for a batch that is
    // told nothing.
    const KEPT: u8 = 10;
    restate(&mut world, KEPT, FLAT_TYPE, &[]);
    let tx = world.begin();
    apply(&world, &tx, &record(KEPT, b"r", b"x"), false).expect("a flat record, and no node");
    let refused = apply(&world, &tx, &node(&world, &tx, KEPT, b"n"), false);
    assert_eq!(refusal(refused), not_merk(KEPT));
    apply(&world, &tx, &node(&world, &tx, KEPT, b"n"), true).expect("told, the node is taken");
}

/// The tree type that decides is the one the batch leaves the subtree
/// with: a batch that states the subtree anew is judged by the tree type
/// it states, and one that clears the subtree and writes both into it
/// by the tree type of the entry.
#[test]
fn the_tree_type_is_the_one_the_batch_leaves_the_subtree_with() {
    let (db, versions) = Db::new(128);
    let mut world = store(&db, &versions);
    // One subtree of a Merk's type that a batch states anew as one that
    // keeps flat records, and one the other way.
    const TO_FLAT: u8 = 9;
    const TO_MERK: u8 = 10;
    restate(&mut world, TO_MERK, FLAT_TYPE, &[]);
    let anew = |name: u8, tree_type: &[u8]| {
        OwnedSubtree::Convert(model::subtree(name), tree_type.to_vec(), None)
    };
    let tx = world.begin();
    for name in [TO_FLAT, TO_MERK] {
        apply(&world, &tx, &record(name, b"r", b"x"), true).expect("a flat record, and no node");
    }
    let mut batch = node(&world, &tx, TO_MERK, b"n");
    batch.subtrees.push(anew(TO_MERK, TREE_TYPE));
    assert_eq!(refusal(apply(&world, &tx, &batch, true)), not_merk(TO_MERK));
    let mut batch = node(&world, &tx, TO_FLAT, b"n");
    batch.subtrees.push(anew(TO_FLAT, FLAT_TYPE));
    apply(&world, &tx, &batch, true).expect("stated anew as a tree that keeps flat records");
    assert_eq!(shown(&tx, TO_FLAT, b"n"), a_node());
    assert_eq!(shown(&tx, TO_FLAT, b"r"), Ok(Some(b"x".to_vec())));
    // A flat record where the Merk has a node, in a batch that states
    // the subtree anew as a tree of a Merk's type.
    let mut batch = record(TO_FLAT, b"r2", b"x");
    batch.subtrees.push(anew(TO_FLAT, TREE_TYPE));
    assert_eq!(refusal(apply(&world, &tx, &batch, true)), not_flat(TO_FLAT));

    // A subtree of a Merk's type that a batch clears and writes a node
    // and a flat record into: where it is laid out, and by key.
    let mut batch = both(&world, &tx, MERK);
    batch
        .subtrees
        .push(OwnedSubtree::Clear(model::subtree(MERK)));
    assert_eq!(refusal(apply(&world, &tx, &batch, true)), not_flat(MERK));
    let first = tx.txn.scope().expect("a scope");
    let beside = tx.txn.scope().expect("a scope beside it");
    assert_eq!(refusal(stated(&beside, &batch, true)), not_flat(MERK));
    // And by key a batch with both and no clear, given the tree types or
    // nothing, under either tree type.
    const EMPTY: u8 = 12;
    restate(&mut world, EMPTY + 1, FLAT_TYPE, &[&tx]);
    for name in [EMPTY, EMPTY + 1] {
        for told in [true, false] {
            let batch = both(&world, &tx, name);
            assert_eq!(refusal(stated(&beside, &batch, told)), not_flat(name));
        }
    }
    beside.rollback();
    first.rollback();
}

/// A batch that puts the node `n1` into the Merk of `name` and the flat
/// record `r1` into its data column.
fn both<'s>(world: &World<'s>, tx: &Tx<'s>, name: u8) -> OwnedBatch {
    let mut batch = node(world, tx, name, b"n1");
    batch.records.push(put(data(name), b"r1", b"beside"));
    batch
}

/// One batch that holds a change set of a Merk and a flat record of the
/// Merk's data column is taken where it is given the tree types that
/// keep flat records and the subtree is of one, and refused otherwise:
/// under another tree type where it is laid out, and before that where
/// it is told nothing.
#[test]
fn one_batch_writes_both_under_a_tree_that_keeps_flat_records() {
    let (db, versions) = Db::new(128);
    let world = store(&db, &versions);
    let tx = world.begin();
    let refused = apply(&world, &tx, &both(&world, &tx, MERK), true);
    assert_eq!(refusal(refused), not_flat(MERK));
    let refused = apply(&world, &tx, &both(&world, &tx, FLAT), false);
    assert_eq!(refusal(refused), not_flat(FLAT));
    apply(&world, &tx, &both(&world, &tx, FLAT), true).expect("both, in one batch");
    assert_eq!(shown(&tx, FLAT, b"r1"), Ok(Some(b"beside".to_vec())));
    assert_eq!(shown(&tx, FLAT, b"n1"), a_node());
}

/// A batch that is taken by key is held to what a batch that is given
/// nothing is held to: nothing asks the tree type of a subtree there. In
/// a transaction that a commit has met, the refusal of a batch that is
/// given the tree types is a conflict, and that of one that is given
/// nothing is the refusal as it is. In a scope whose writes are not laid
/// out it is the refusal as it is for both, and the scope has the batch
/// taken once its writes are laid out.
#[test]
fn by_key_a_batch_is_held_to_what_one_that_is_given_nothing_is() {
    let (db, versions) = Db::new(128);
    let mut world = store(&db, &versions);
    const BARE: u8 = 9;
    let tx = world.begin();
    apply(&world, &tx, &node(&world, &tx, MERK, b"n1"), true).expect("a node");
    apply(&world, &tx, &node(&world, &tx, FLAT, b"n1"), true).expect("a node");
    assert!(committed(world.commit(tx, &[])));

    let tx = world.begin();
    world.write(&tx, vec![put(aux(1), b"x", b"mine")]);
    apply(&world, &tx, &record(BARE, b"r", b"mine"), true).expect("a flat record, and no node");
    let other = OwnedBatch {
        records: vec![put(aux(1), b"x", b"theirs")],
        ..OwnedBatch::default()
    };
    assert!(committed(world.write_outside(&other, &[&tx])));
    // The next call takes the commit in.
    assert_eq!(shown(&tx, FLAT, b"n1"), a_node());
    assert_eq!(tx.txn.stats().base, None, "it serves its records by key");

    let by_key = |name: u8| BatchError::BothByKey {
        subtree: model::subtree(name),
    };
    assert!(by_key(FLAT).is_conflict());
    let refused = apply(&world, &tx, &record(FLAT, b"r1", b"by key"), true);
    assert_eq!(refusal(refused), by_key(FLAT));
    // The tree type is not asked.
    let refused = apply(&world, &tx, &record(MERK, b"r1", b"by key"), true);
    assert_eq!(refusal(refused), by_key(MERK));
    // A change set where the transaction wrote a flat record, and one
    // batch with both.
    let refused = apply(&world, &tx, &node(&world, &tx, BARE, b"n"), true);
    assert_eq!(refusal(refused), by_key(BARE));
    const EMPTY: u8 = 10;
    let refused = apply(&world, &tx, &both(&world, &tx, EMPTY), true);
    assert_eq!(refusal(refused), by_key(EMPTY));
    // A batch that is given nothing is refused as it is.
    let refused = apply(&world, &tx, &record(FLAT, b"r2", b"by key"), false);
    assert_eq!(refusal(refused), not_flat(FLAT));
    let refused = apply(&world, &tx, &node(&world, &tx, BARE, b"n"), false);
    assert_eq!(refusal(refused), not_merk(BARE));
    assert_eq!(shown(&tx, FLAT, b"r1"), Ok(None));
    assert_eq!(tx.txn.stats().base, None);
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);

    // A scope that is opened beside another: no commit has met the
    // transaction, and its batch is refused as one that is given nothing.
    let tx = world.begin();
    let first = tx.txn.scope().expect("a scope");
    let beside = tx.txn.scope().expect("a scope beside it");
    assert!(!beside.is_laid_out());
    let batch = record(FLAT, b"r1", b"beside");
    assert_eq!(refusal(stated(&beside, &batch, true)), not_flat(FLAT));
    first.merge();
    beside.lay_out().expect("it has written nothing");
    stated(&beside, &batch, true).expect("laid out, the record goes beside the nodes");
    beside.merge();
    assert_eq!(shown(&tx, FLAT, b"r1"), Ok(Some(b"beside".to_vec())));
    commit_untold(&db, &versions, tx);
}

/// A scope that is opened beside another keeps its writes in the write
/// set, and they are laid out later ([`Scope::lay_out`]): each batch is
/// judged there by the tree types it was given, whatever the batch that
/// the transaction's pages took last was given, and also after the
/// rollback of the other scope made the write set again.
///
/// The scope puts a flat record into a subtree whose Merk has no node,
/// which by key any batch may. A commit then gives the Merk its first
/// node, so that the pages have the record to judge.
#[test]
fn a_scope_that_is_laid_out_later_is_judged_by_what_its_batch_was_given() {
    const OTHER: u8 = 9;
    for (told, told_last, merged) in [
        (true, true, true),
        (true, false, true),
        (false, true, true),
        (false, false, true),
        (true, false, false),
        (true, true, false),
    ] {
        let (db, versions) = Db::new(128);
        let mut world = store(&db, &versions);
        let tx = world.begin();
        let first = tx.txn.scope().expect("a scope");
        stated(&first, &node(&world, &tx, MERK, b"a"), told_last).expect("a node");
        let beside = tx.txn.scope().expect("a scope beside it");
        assert!(!beside.is_laid_out());
        stated(&beside, &record(FLAT, b"r1", b"beside"), told).expect("a flat record, and no node");
        let first_node = |items: &mut BTreeMap<Vec<u8>, Vec<u8>>| {
            items.insert(b"n1".to_vec(), vec![3; 20]);
        };
        assert!(committed(world.edit_outside(FLAT, first_node, &[&tx])));
        // The pages are made again over that commit and take one more
        // batch, which is given the tree types or is not.
        stated(&first, &node(&world, &tx, OTHER, b"b"), told_last).expect("a node");
        assert!(tx.txn.stats().base.is_some(), "the writes are laid out");
        match merged {
            true => first.merge(),
            false => first.rollback(),
        }

        let laid = beside.lay_out();
        if !told {
            let refused = laid.expect_err("a batch that was given nothing");
            let refused = refusal(Err(refused));
            assert_eq!(refused, not_flat(FLAT), "{told_last} {merged}");
            assert!(!beside.is_laid_out());
            beside.rollback();
            tx.txn.rollback().expect("no scope is open");
            tx.forget();
            continue;
        }
        laid.unwrap_or_else(|refused| panic!("{told_last} {merged}: {refused}"));
        assert!(beside.is_laid_out());
        assert!(tx.txn.stats().base.is_some(), "the pages are kept");
        beside.merge();
        assert_eq!(shown(&tx, FLAT, b"r1"), Ok(Some(b"beside".to_vec())));
        assert_eq!(shown(&tx, FLAT, b"n1"), a_node());
        commit_untold(&db, &versions, tx);
    }
}

/// The tree type decides again wherever the batch is laid out again. A
/// transaction puts a flat record beside the nodes of a tree that keeps
/// flat records. Over a commit beside it that writes elsewhere, its
/// writes are laid out again and it reads what it wrote. Over one that
/// states the subtree anew as a tree of a Merk's type, the record is no
/// more taken: the layout is refused, and so is the commit, with the
/// transaction as it was. The same holds of a change set where the
/// transaction wrote flat records.
#[test]
fn the_tree_type_decides_again_where_the_batch_is_laid_out_again() {
    let (db, versions) = Db::new(128);
    let mut world = store(&db, &versions);
    let tx = world.begin();
    apply(&world, &tx, &node(&world, &tx, FLAT, b"n1"), true).expect("a node");
    assert!(committed(world.commit(tx, &[])));

    let tx = world.begin();
    apply(&world, &tx, &record(FLAT, b"r1", b"kept"), true).expect("a flat record");
    let elsewhere = OwnedBatch {
        records: vec![put(aux(1), b"y", b"theirs")],
        ..OwnedBatch::default()
    };
    assert!(committed(world.write_outside(&elsewhere, &[&tx])));
    assert_eq!(shown(&tx, FLAT, b"r1"), Ok(Some(b"kept".to_vec())));
    assert!(tx.txn.stats().base.is_some(), "laid out over the commit");

    restate(&mut world, FLAT, TREE_TYPE, &[&tx]);
    let mut out = Vec::new();
    (tx.txn.get(aux(1), b"y", &mut out)).expect("a read by key");
    assert_eq!(tx.txn.stats().base, None, "the layout is refused");
    let refused = commit_refused(&db, &versions, tx);
    assert!(
        matches!(&refused, TxnError::Batch(refused) if *refused == not_flat(FLAT)),
        "{refused:?}"
    );
    assert!(!refused.is_conflict());

    // A change set where the transaction wrote a flat record: the Merk
    // has no node when the record is written, and the subtree is stated
    // anew before the writes are laid out again.
    const LATER: u8 = 9;
    restate(&mut world, LATER, FLAT_TYPE, &[]);
    let tx = world.begin();
    apply(&world, &tx, &record(LATER, b"r1", b"kept"), true).expect("a flat record");
    apply(&world, &tx, &node(&world, &tx, LATER, b"n1"), true)
        .expect("the first node, beside the flat record");
    restate(&mut world, LATER, TREE_TYPE, &[&tx]);
    (tx.txn.get(aux(1), b"y", &mut out)).expect("a read by key");
    assert_eq!(tx.txn.stats().base, None, "the layout is refused");
    let refused = commit_refused(&db, &versions, tx);
    assert!(
        matches!(&refused, TxnError::Batch(refused) if *refused == not_merk(LATER)),
        "{refused:?}"
    );
    assert!(!refused.is_conflict());
}

/// Once a transaction holds nodes and flat records in one data column, a
/// batch that is given nothing is refused there whichever kind it
/// writes, and whichever of the two kinds has the lower key: where it is
/// laid out, and by key. By key a batch that is given the tree types is
/// refused as well.
#[test]
fn a_batch_that_is_given_nothing_is_refused_where_the_transaction_holds_both() {
    for (of_node, of_record) in [(&b"a"[..], &b"z"[..]), (&b"z"[..], &b"a"[..])] {
        let (db, versions) = Db::new(128);
        let mut world = store(&db, &versions);
        let tx = world.begin();
        apply(&world, &tx, &record(FLAT, of_record, b"x"), true).expect("a flat record");
        apply(&world, &tx, &node(&world, &tx, FLAT, of_node), true).expect("a node beside it");
        let more = node(&world, &tx, FLAT, b"m");
        let refused = apply(&world, &tx, &more, false);
        assert_eq!(refusal(refused), not_merk(FLAT));
        let refused = apply(&world, &tx, &record(FLAT, b"s", b"x"), false);
        assert_eq!(refusal(refused), not_flat(FLAT));

        let first = tx.txn.scope().expect("a scope");
        let beside = tx.txn.scope().expect("a scope beside it");
        let by_key = |when: &str| {
            for told in [false, true] {
                let refused = refusal(stated(&beside, &more, told));
                assert_eq!(refused, not_merk(FLAT), "{when}");
                let refused = stated(&beside, &record(FLAT, b"s", b"x"), told);
                assert_eq!(refusal(refused), not_flat(FLAT), "{when}");
            }
        };
        by_key("beside another scope");
        // The write set is made again without the writes of a scope that
        // is rolled back, and holds what it held.
        let elsewhere = |name: u8| OwnedBatch {
            records: vec![put(aux(name), b"x", b"elsewhere")],
            ..OwnedBatch::default()
        };
        stated(&first, &elsewhere(1), false).expect("a record elsewhere");
        stated(&beside, &elsewhere(2), false).expect("a record elsewhere, by key");
        first.rollback();
        by_key("after the rollback of the other scope");
        beside.rollback();
        assert_eq!(shown(&tx, FLAT, b"m"), Ok(None));
        assert_eq!(shown(&tx, FLAT, b"s"), Ok(None));
        assert!(committed(world.commit(tx, &[])));
    }
}

/// What a transaction wrote into the data column of a subtree goes with
/// a clear of the subtree, as for a batch that is given nothing: the
/// batch that clears it writes the other kind into it, and so does a
/// batch after the clear, under a tree type that keeps no flat records.
#[test]
fn a_cleared_subtree_takes_the_other_kind() {
    const NODE_FIRST: u8 = 9;
    const RECORD_FIRST: u8 = 10;
    const NODE_AND_A_CLEAR: u8 = 11;
    const RECORD_AND_A_CLEAR: u8 = 12;
    let cleared = |name: u8| OwnedSubtree::Clear(model::subtree(name));
    for told in [true, false] {
        let (db, versions) = Db::new(128);
        let world = store(&db, &versions);
        let tx = world.begin();
        for name in [NODE_FIRST, NODE_AND_A_CLEAR] {
            apply(&world, &tx, &node(&world, &tx, name, b"n"), told).expect("a node");
        }
        for name in [RECORD_FIRST, RECORD_AND_A_CLEAR] {
            apply(&world, &tx, &record(name, b"r", b"x"), told).expect("a flat record");
        }
        // The clear and the other kind in one batch.
        let mut batch = record(NODE_FIRST, b"r", b"x");
        batch.subtrees.push(cleared(NODE_FIRST));
        apply(&world, &tx, &batch, told).expect("a flat record of the cleared subtree");
        let mut batch = node(&world, &tx, RECORD_FIRST, b"n");
        batch.subtrees.push(cleared(RECORD_FIRST));
        apply(&world, &tx, &batch, told).expect("a node of the cleared subtree");
        // The clear in a batch of its own, and the other kind after it.
        for name in [NODE_AND_A_CLEAR, RECORD_AND_A_CLEAR] {
            apply(&world, &tx, &subtrees(vec![cleared(name)]), told).expect("a clear");
        }
        apply(&world, &tx, &record(NODE_AND_A_CLEAR, b"r", b"x"), told)
            .expect("a flat record after the clear");
        let batch = node(&world, &tx, RECORD_AND_A_CLEAR, b"n");
        apply(&world, &tx, &batch, told).expect("a node after the clear");
        for name in [NODE_FIRST, NODE_AND_A_CLEAR] {
            assert_eq!(shown(&tx, name, b"n"), Ok(None), "{told}");
            assert_eq!(flat(&tx, name, b"r"), Ok(Some(b"x".to_vec())), "{told}");
        }
        for name in [RECORD_FIRST, RECORD_AND_A_CLEAR] {
            assert_eq!(flat(&tx, name, b"r"), Ok(None), "{told}");
            assert_eq!(shown(&tx, name, b"n"), a_node(), "{told}");
        }
    }
}

/// The kinds a transaction wrote into a data column are those of all its
/// writes, also of one that a later write under the same key took the
/// place of: a node over the flat record under its key, and a flat record
/// where a node was removed. By key, and where it is laid out, a batch
/// that is given nothing is refused the other kind there.
#[test]
fn what_was_written_under_a_key_before_is_held_too() {
    let (db, versions) = Db::new(128);
    let mut world = store(&db, &versions);
    let tx = world.begin();
    apply(&world, &tx, &record(FLAT, b"k", b"x"), true).expect("a flat record");
    apply(&world, &tx, &node(&world, &tx, FLAT, b"k"), true).expect("the node takes it");
    let more = node(&world, &tx, FLAT, b"m");
    let first = tx.txn.scope().expect("a scope");
    let beside = tx.txn.scope().expect("a scope beside it");
    for told in [false, true] {
        assert_eq!(refusal(stated(&beside, &more, told)), not_merk(FLAT));
    }
    beside.rollback();
    first.rollback();
    assert_eq!(refusal(apply(&world, &tx, &more, false)), not_merk(FLAT));
    assert!(committed(world.commit(tx, &[])));

    let tx = world.begin();
    let removed = OwnedBatch {
        merks: vec![world.merk(&tx, FLAT, |items| {
            items.remove(&b"k"[..]);
        })],
        ..OwnedBatch::default()
    };
    apply(&world, &tx, &removed, true).expect("the node is removed");
    apply(&world, &tx, &record(FLAT, b"k", b"again"), true).expect("a flat record");
    let range = OwnedBatch {
        records: vec![Write::Range(data(FLAT), b"a".to_vec(), None)],
        ..OwnedBatch::default()
    };
    let first = tx.txn.scope().expect("a scope");
    let beside = tx.txn.scope().expect("a scope beside it");
    for told in [false, true] {
        assert_eq!(refusal(stated(&beside, &range, told)), not_flat(FLAT));
    }
    beside.rollback();
    first.rollback();
    assert_eq!(refusal(apply(&world, &tx, &range, false)), not_flat(FLAT));
}

/// What the pages know of a subtree is of the subtree as a batch has it.
/// A subtree that went with the drop of one above it, and that one batch
/// enters again and writes, is new in that batch: it takes either kind,
/// whatever the transaction wrote into the subtree that was there, as it
/// does where the enter and the write are two batches.
#[test]
fn a_subtree_that_is_entered_again_holds_nothing_of_the_one_that_went() {
    for (told, node_first, one_batch) in [
        (true, true, true),
        (true, false, true),
        (false, true, true),
        (true, true, false),
        (true, false, false),
    ] {
        let case = format!("told {told}, a node first {node_first}, one batch {one_batch}");
        let (db, versions) = Db::new(128);
        let mut world = World::new(&db, &versions);
        let tx = world.begin();
        planted(&world, &tx);
        assert!(committed(world.commit(tx, &[])));
        restate(&mut world, INNER, FLAT_TYPE, &[]);

        let tx = world.begin();
        // The subtree has nodes. The transaction writes one more, or a
        // flat record beside them.
        let before = match node_first {
            true => node(&world, &tx, INNER, b"three"),
            false => record(INNER, b"r", b"x"),
        };
        apply(&world, &tx, &before, true).expect("a write of the subtree");
        let gone = subtrees(vec![OwnedSubtree::Drop(model::subtree(OUTER))]);
        apply(&world, &tx, &gone, told).expect("the subtree above is dropped");
        // Entered again with no host, as a Merk: the other kind.
        let mut after = match node_first {
            true => record(INNER, b"r", b"x"),
            false => node(&world, &tx, INNER, b"n"),
        };
        match one_batch {
            true => after.subtrees.push(enter(INNER)),
            false => {
                let entered = subtrees(vec![enter(INNER)]);
                apply(&world, &tx, &entered, told).expect("entered again");
            }
        }
        apply(&world, &tx, &after, told).unwrap_or_else(|refused| panic!("{case}: {refused}"));
        match node_first {
            true => assert_eq!(flat(&tx, INNER, b"r"), Ok(Some(b"x".to_vec())), "{case}"),
            false => assert_eq!(shown(&tx, INNER, b"n"), a_node(), "{case}"),
        }
    }
}

/// A subtree that went with the drop of one above it is not entered: a
/// batch that writes it is refused as that, whatever the transaction
/// wrote into the subtree that was there.
#[test]
fn a_subtree_that_went_with_one_above_it_is_refused_as_not_entered() {
    for told in [true, false] {
        let (db, versions) = Db::new(128);
        let mut world = World::new(&db, &versions);
        let tx = world.begin();
        planted(&world, &tx);
        assert!(committed(world.commit(tx, &[])));
        restate(&mut world, INNER, FLAT_TYPE, &[]);
        let tx = world.begin();
        apply(&world, &tx, &record(INNER, b"r", b"x"), true).expect("a flat record");
        let gone = subtrees(vec![OwnedSubtree::Drop(model::subtree(OUTER))]);
        apply(&world, &tx, &gone, told).expect("the subtree above is dropped");
        let not_entered = BatchError::NotEntered {
            subtree: model::subtree(INNER),
        };
        let refused = apply(&world, &tx, &node(&world, &tx, INNER, b"n"), told);
        assert_eq!(refusal(refused), not_entered, "{told}");
        let refused = apply(&world, &tx, &record(INNER, b"s", b"x"), told);
        assert_eq!(refusal(refused), not_entered, "{told}");
        // Of two subtrees that are not entered, the batch is refused for
        // the one that it would be refused for with nothing written.
        let mut two = node(&world, &tx, NEVER, b"n");
        two.merks.extend(node(&world, &tx, INNER, b"n").merks);
        let never = BatchError::NotEntered {
            subtree: model::subtree(NEVER),
        };
        assert_eq!(refusal(apply(&world, &tx, &two, told)), never, "{told}");
    }
}

/// A change set that writes no node and removes none says nothing of
/// what its subtree holds: a batch that is given nothing puts a flat
/// record there afterwards, as before.
#[test]
fn a_change_set_with_no_node_leaves_a_subtree_one_with_no_nodes() {
    const BARE: u8 = 9;
    let (db, versions) = Db::new(128);
    let world = store(&db, &versions);
    for told in [false, true] {
        let tx = world.begin();
        let nothing = world.merk(&tx, BARE, |_| {});
        assert!(nothing.nodes.is_empty() && nothing.removed.is_empty());
        let batch = OwnedBatch {
            merks: vec![nothing],
            records: vec![put(aux(1), b"x", b"elsewhere")],
            ..OwnedBatch::default()
        };
        apply(&world, &tx, &batch, told).expect("a change set of nothing");
        apply(&world, &tx, &record(BARE, b"r", b"x"), false).expect("a flat record, and no node");
        assert_eq!(flat(&tx, BARE, b"r"), Ok(Some(b"x".to_vec())));
        tx.txn.rollback().expect("no scope is open");
        tx.forget();
    }
}

/// By key a batch that is given the tree types is held to the rule that
/// one batch writes no node and no flat record of one subtree where that
/// rule stands for a batch that is given nothing: before the batch is
/// asked for a claim, so that it takes none and is not told of another
/// scope's, and before it is told that it writes a key twice.
#[test]
fn by_key_one_batch_with_both_is_refused_before_it_claims_anything() {
    const EMPTY: u8 = 9;
    let (db, versions) = Db::new(128);
    let mut world = store(&db, &versions);
    for name in [EMPTY, FLAT] {
        let tx = world.begin();
        let batch = both(&world, &tx, name);
        let mut one_key = node(&world, &tx, name, b"k");
        one_key.records.push(put(data(name), b"k", b"x"));
        for told in [false, true] {
            let first = tx.txn.scope().expect("a scope");
            let beside = tx.txn.scope().expect("a scope beside it");
            assert_eq!(refusal(stated(&beside, &batch, told)), not_flat(name));
            assert_eq!(refusal(stated(&beside, &one_key, told)), not_flat(name));
            // It took no claim: the other scope writes the subtree.
            stated(&first, &record(name, b"z", b"first"), false).expect("no claim stands");
            // And it is told of no claim of the other scope.
            assert_eq!(refusal(stated(&beside, &batch, told)), not_flat(name));
            beside.rollback();
            first.rollback();
        }
        // Laid out, the same key twice is that.
        let twice = refusal(apply(&world, &tx, &one_key, true));
        assert_eq!(twice, BatchError::KeyTwice { unit: data(name) });
        tx.txn.rollback().expect("no scope is open");
        tx.forget();
    }

    // And in a transaction that a commit has met it is the conflict.
    let tx = world.begin();
    world.write(&tx, vec![put(aux(1), b"x", b"mine")]);
    let other = OwnedBatch {
        records: vec![put(aux(1), b"x", b"theirs")],
        ..OwnedBatch::default()
    };
    assert!(committed(world.write_outside(&other, &[&tx])));
    let mut one_key = node(&world, &tx, EMPTY, b"k");
    one_key.records.push(put(data(EMPTY), b"k", b"x"));
    let by_key = BatchError::BothByKey {
        subtree: model::subtree(EMPTY),
    };
    assert_eq!(refusal(apply(&world, &tx, &one_key, true)), by_key);
    assert_eq!(
        refusal(apply(&world, &tx, &one_key, false)),
        not_flat(EMPTY)
    );
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);
}

/// A cursor of records stands on a node that its transaction wrote into
/// the data column and is refused the node's value, as it was before a
/// subtree could hold both. Once the node is committed the cursor shows
/// the flat records alone.
#[test]
fn a_cursor_of_records_stands_on_the_nodes_of_its_transaction() {
    let (db, versions) = Db::new(128);
    let mut world = store(&db, &versions);
    let tx = world.begin();
    apply(&world, &tx, &record(FLAT, b"k", b"taken"), true).expect("a flat record");
    apply(&world, &tx, &record(FLAT, b"z", b"kept"), true).expect("a flat record");
    apply(&world, &tx, &node(&world, &tx, FLAT, b"k"), true).expect("the node takes one");
    apply(&world, &tx, &node(&world, &tx, FLAT, b"m"), true).expect("a node between them");
    let mut cursor = tx.txn.cursor(data(FLAT));
    cursor.seek_to_first().expect("a seek");
    for key in [b"k", b"m"] {
        assert_eq!(cursor.key(), Some(&key[..]));
        let refused = cursor
            .value(&mut Vec::new())
            .expect_err("a node of the transaction")
            .to_string();
        assert!(
            refused.contains("a Merk node under a cursor of records"),
            "{refused}"
        );
        cursor.next().expect("a step");
    }
    assert_eq!(cursor.key(), Some(&b"z"[..]));
    let mut value = Vec::new();
    assert!(cursor.value(&mut value).expect("a flat record"));
    assert_eq!(value, b"kept");
    drop(cursor);
    assert!(committed(world.commit(tx, &[])));
    let tx = world.begin();
    let mut cursor = tx.txn.cursor(data(FLAT));
    let kept = vec![(b"z".to_vec(), b"kept".to_vec())];
    for back in [false, true] {
        assert_eq!(scan(&mut cursor, back), kept, "{back}");
    }
    drop(cursor);
    tx.txn.rollback().expect("no scope is open");
    tx.forget();
}

/// The write set is made again without the writes of a scope that is
/// rolled back, and knows of the operations that stay what it knew: that
/// one of them was taken by the tree type of a subtree, so that both
/// kinds are looked for afterwards, whichever has the lower key.
#[test]
fn the_rollback_of_a_scope_leaves_what_is_known_of_the_other_operations() {
    for (of_node, of_record) in [(&b"a"[..], &b"z"[..]), (&b"z"[..], &b"a"[..])] {
        let (db, versions) = Db::new(128);
        let world = store(&db, &versions);
        let tx = world.begin();
        let first = tx.txn.scope().expect("a scope");
        let elsewhere = OwnedBatch {
            records: vec![put(aux(1), b"x", b"elsewhere")],
            ..OwnedBatch::default()
        };
        stated(&first, &elsewhere, false).expect("a record elsewhere");
        // A scope beside it whose writes are laid out as it makes them.
        let beside = tx.txn.scope().expect("a scope beside it");
        beside.lay_out().expect("it has written nothing");
        stated(&beside, &record(FLAT, of_record, b"x"), true).expect("a flat record");
        stated(&beside, &node(&world, &tx, FLAT, of_node), true).expect("a node beside it");
        first.rollback();
        beside.merge();

        let more = node(&world, &tx, FLAT, b"m");
        let first = tx.txn.scope().expect("a scope");
        let beside = tx.txn.scope().expect("a scope beside it");
        for told in [false, true] {
            assert_eq!(refusal(stated(&beside, &more, told)), not_merk(FLAT));
            let refused = stated(&beside, &record(FLAT, b"s", b"x"), told);
            assert_eq!(refusal(refused), not_flat(FLAT));
        }
        beside.rollback();
        first.rollback();
        tx.txn.rollback().expect("no scope is open");
        tx.forget();
    }
}
