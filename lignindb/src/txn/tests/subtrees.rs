//! Subtrees that a batch enters, drops and clears, and what the subtree
//! directory is told of the roots a write changed (`lignin_tree::directory`,
//! `transactions.md` CV9, CF5, `tree-layout.md` S3, D1, D4).

use std::sync::atomic::Ordering;

use lignin_format::{tree_page::Side, CellAddress, PageId, SlotId};
use lignin_store::cache::OwnerId;
use lignin_tree::{
    directory::{DirectoryError, Root, RootChange},
    flat::Private,
};

use super::{
    model::{self, value, OwnedBatch, OwnedMerk, OwnedSubtree},
    store::{Db, Outcome, Rng, ENTERED, TREE_TYPE},
    *,
};
use crate::txn::frames::{probe, root_changes};

/// A subtree that no store of the tests has entered, and one that its node
/// `host` hosts.
const OUTER: u8 = ENTERED + 10;
const INNER: u8 = ENTERED + 11;

/// The keys and values of one Merk.
type Items = BTreeMap<Vec<u8>, Vec<u8>>;

fn aux(name: u8) -> Unit {
    model::unit(Column::Aux, name)
}

fn enter(name: u8) -> OwnedSubtree {
    OwnedSubtree::Enter(model::subtree(name), None)
}

fn hosted(name: u8, host: u8, key: &[u8]) -> OwnedSubtree {
    let host = (model::subtree(host), key.to_vec(), 0);
    OwnedSubtree::Enter(model::subtree(name), Some(host))
}

fn subtrees(subtrees: Vec<OwnedSubtree>) -> OwnedBatch {
    OwnedBatch {
        subtrees,
        ..OwnedBatch::default()
    }
}

fn batch_error(result: Result<Applied, TxnError>) -> BatchError {
    match result {
        Err(TxnError::Batch(error)) => error,
        other => panic!("a refused batch is expected, and not {other:?}"),
    }
}

fn node_count(tx: &Tx<'_>, name: u8, keys: &[&[u8]]) -> usize {
    let found = keys.iter().filter(|key| {
        (tx.txn
            .node(model::subtree(name), key, |node| node.is_some()))
        .expect("a read")
    });
    found.count()
}

/// A batch that enters `OUTER` with three nodes and flat records beside
/// them, and `INNER` under its node `host`, with two nodes and flat
/// records of its own.
fn planted<'s>(world: &World<'s>, tx: &Tx<'s>) {
    let mut outer = world.merk(tx, OUTER, |items| {
        items.insert(b"host".to_vec(), b"tree".to_vec());
        items.insert(b"left".to_vec(), vec![1; 40]);
        items.insert(b"right".to_vec(), vec![2; 300]);
    });
    (outer.hosted).insert(b"host".to_vec(), vec![model::subtree(INNER)]);
    let inner = world.merk(tx, INNER, |items| {
        items.insert(b"one".to_vec(), vec![3; 20]);
        items.insert(b"two".to_vec(), vec![4; 20]);
    });
    let batch = OwnedBatch {
        merks: vec![inner, outer],
        records: vec![
            put(aux(OUTER), b"a", b"1"),
            put(model::unit(Column::Meta, OUTER), b"m", b"2"),
            put(model::unit(Column::Roots, OUTER), b"r", b"3"),
            put(aux(INNER), b"ia", b"7"),
            put(model::unit(Column::Meta, INNER), b"im", b"8"),
        ],
        subtrees: vec![enter(OUTER), hosted(INNER, OUTER, b"host")],
    };
    world.apply(tx, &batch).expect("the subtrees are made");
}

/// Drops `OUTER` through `tx`. The drop names that subtree alone: the
/// model works out what is below it, as the transaction has to.
fn drop_outer<'s>(world: &World<'s>, tx: &Tx<'s>) {
    let batch = subtrees(vec![OwnedSubtree::Drop(model::subtree(OUTER))]);
    world.apply(tx, &batch).expect("the subtree is dropped");
}

/// A change of `INNER` through `tx`, with its host written in the same
/// batch, which names its root again.
fn inner_batch<'s>(world: &World<'s>, tx: &Tx<'s>, edit: impl FnOnce(&mut Items)) -> OwnedBatch {
    let inner = world.merk(tx, INNER, edit);
    let stamp = inner.nodes.len() as u8;
    let mut outer = world.merk(tx, OUTER, |items| {
        let mut value = items.get(&b"host"[..]).cloned().unwrap_or_default();
        value.push(stamp);
        items.insert(b"host".to_vec(), value);
    });
    (outer.hosted).insert(b"host".to_vec(), vec![model::subtree(INNER)]);
    OwnedBatch {
        merks: vec![inner, outer],
        ..OwnedBatch::default()
    }
}

/// The flat records that [`planted`] puts under `OUTER` and `INNER`.
fn planted_records() -> [(Unit, &'static [u8]); 5] {
    [
        (aux(OUTER), b"a"),
        (model::unit(Column::Meta, OUTER), b"m"),
        (model::unit(Column::Roots, OUTER), b"r"),
        (aux(INNER), b"ia"),
        (model::unit(Column::Meta, INNER), b"im"),
    ]
}

/// How many of the records [`planted`] made a transaction that has written
/// nothing reads now: by key, as committed records, and under a cursor.
fn planted_read(world: &World<'_>) -> usize {
    let reader = world.begin();
    let mut found = 0;
    for (unit, key) in planted_records() {
        let mut out = Vec::new();
        let read = reader.txn.get(unit, key, &mut out).expect("a read");
        let committed = (reader.txn.committed_get(unit, key, &mut out)).expect("a read");
        let mut cursor = reader.txn.cursor(unit);
        cursor.seek(key).expect("a seek");
        let under = cursor.key() == Some(key);
        assert!(read == committed && read == under, "{unit}: one answer");
        found += usize::from(read);
    }
    found
}

fn census(db: &Db) -> (u64, u64, u64) {
    *db.counts.census.lock().expect("a census")
}

#[test]
fn a_batch_enters_the_subtrees_it_writes() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let tx = world.begin();
    let scope = tx.txn.scope().expect("a scope");
    let apply = |batch: &OwnedBatch| batch.with(|batch| scope.apply(batch));

    // A change set names a subtree that is entered.
    let unknown = world.merk(&tx, OUTER, |items| {
        drop(items.insert(b"n".to_vec(), b"v".to_vec()))
    });
    let batch = OwnedBatch {
        merks: vec![unknown],
        ..OwnedBatch::default()
    };
    let subtree = model::subtree(OUTER);
    assert_eq!(
        batch_error(apply(&batch)),
        BatchError::NotEntered { subtree }
    );
    // A subtree is entered once, by one batch, and below one that is there.
    let twice = subtrees(vec![enter(OUTER), enter(OUTER)]);
    assert_eq!(
        batch_error(apply(&twice)),
        BatchError::SubtreeTwice { subtree }
    );
    let gone = subtrees(vec![
        OwnedSubtree::Drop(model::subtree(1)),
        OwnedSubtree::Clear(model::subtree(1)),
    ]);
    assert_eq!(
        batch_error(apply(&gone)),
        BatchError::SubtreeTwice {
            subtree: model::subtree(1)
        }
    );
    assert_eq!(
        batch_error(apply(&subtrees(vec![enter(1)]))),
        BatchError::Entered {
            subtree: model::subtree(1)
        }
    );
    assert_eq!(
        batch_error(apply(&subtrees(vec![hosted(INNER, OUTER, b"host")]))),
        BatchError::NotEntered { subtree }
    );
    for leaves in [OwnedSubtree::Drop(subtree), OwnedSubtree::Clear(subtree)] {
        assert_eq!(
            batch_error(apply(&subtrees(vec![leaves]))),
            BatchError::NotEntered { subtree }
        );
    }
    let long = [7; 256];
    let entered = |tree_type, key| SubtreeWrite::Enter {
        subtree,
        tree_type,
        host: Some(HostPut {
            subtree: model::subtree(1),
            key,
            position: 0,
        }),
    };
    let raw = |write| {
        let subtrees = [write];
        scope.apply(&Batch {
            subtrees: &subtrees,
            ..Batch::default()
        })
    };
    assert_eq!(
        batch_error(raw(entered(&long, b"k"))),
        BatchError::TreeTypeTooLong { subtree, len: 256 }
    );
    assert_eq!(
        batch_error(raw(entered(TREE_TYPE, &long))),
        BatchError::KeyTooLong {
            subtree: model::subtree(1),
            len: 256
        }
    );
    scope.merge();
    assert_eq!(
        tx.txn.stats(),
        TxnStats::default(),
        "nothing was added, and nothing is held"
    );

    // The batch that writes the host enters the subtree, and may write it.
    planted(&world, &tx);
    world.check(&tx);
    let again = tx.txn.scope().expect("a scope");
    let error = subtrees(vec![enter(INNER)]).with(|batch| again.apply(batch));
    assert_eq!(
        batch_error(error),
        BatchError::Entered {
            subtree: model::subtree(INNER)
        }
    );
    again.merge();
    // What another handle reads has neither.
    let other = world.begin();
    assert_eq!(node_count(&other, OUTER, &[b"host", b"left", b"right"]), 0);
    let merk = world.merk(&other, INNER, |items| {
        drop(items.insert(b"n".to_vec(), b"v".to_vec()))
    });
    let batch = OwnedBatch {
        merks: vec![merk],
        ..OwnedBatch::default()
    };
    assert_eq!(
        batch_error(world.apply(&other, &batch)),
        BatchError::NotEntered {
            subtree: model::subtree(INNER)
        }
    );

    assert!(matches!(world.commit(tx, &[&other]), Outcome::Committed(_)));
    world.check_committed();
    assert_eq!(census(&db), (u64::from(ENTERED) + 2, 0, 0));
    // The other transaction reads them now, and writes the Merk below.
    world.check(&other);
    let merk = world.merk(&other, INNER, |items| {
        drop(items.insert(b"n".to_vec(), b"v".to_vec()))
    });
    // Its host is written with it, and names its root again.
    let mut host = world.merk(&other, OUTER, |items| {
        drop(items.insert(b"host".to_vec(), b"tree again".to_vec()))
    });
    (host.hosted).insert(b"host".to_vec(), vec![model::subtree(INNER)]);
    let batch = OwnedBatch {
        merks: vec![merk, host],
        ..OwnedBatch::default()
    };
    (world.apply(&other, &batch)).expect("the Merk is entered now");
    world.check(&other);
    assert!(matches!(world.commit(other, &[]), Outcome::Committed(_)));
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
}

/// `transactions.md` CV9: a drop is a write of its transaction. From the
/// drop on the transaction reads no record of the subtree and no node of a
/// Merk below it, every other handle reads them until the commit, and a
/// rollback gives them back.
#[test]
fn a_dropped_subtree_is_gone_from_the_drop_on_and_for_the_others_from_its_commit() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let tx = world.begin();
    planted(&world, &tx);
    world.edit(&tx, 3, |items| {
        drop(items.insert(b"beside".to_vec(), vec![9; 30]))
    });
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
    let outer: [&[u8]; 3] = [b"host", b"left", b"right"];
    let inner: [&[u8]; 2] = [b"one", b"two"];

    let tx = world.begin();
    let reader = world.begin();
    world.edit(&tx, 3, |items| {
        drop(items.insert(b"more".to_vec(), vec![8; 30]))
    });
    let before = tx.model.borrow().writes.len();
    assert_eq!(
        (
            node_count(&tx, OUTER, &outer),
            node_count(&tx, INNER, &inner)
        ),
        (3, 2)
    );

    // In a scope that is rolled back: the directory is read again, and the
    // Merk below is opened as before.
    let scope = tx.txn.scope().expect("a scope");
    let dropped = subtrees(vec![OwnedSubtree::Drop(model::subtree(OUTER))]);
    dropped
        .with(|batch| scope.apply(batch))
        .expect("the subtree is dropped");
    let gone = |name, key: &[u8]| {
        (scope.node(model::subtree(name), key, |node| node.is_none())).expect("a read")
    };
    assert!(gone(OUTER, b"left") && gone(INNER, b"one"));
    scope.rollback();
    assert_eq!(
        (
            node_count(&tx, OUTER, &outer),
            node_count(&tx, INNER, &inner)
        ),
        (3, 2)
    );
    world.check(&tx);

    drop_outer(&world, &tx);
    assert_eq!(
        (
            node_count(&tx, OUTER, &outer),
            node_count(&tx, INNER, &inner)
        ),
        (0, 0)
    );
    assert_eq!(tx.txn.stats().writes, before + 1, "a drop is one write");
    // Every record of the subtree, in every column, by key and under a
    // cursor, and the nodes of the Merk below.
    world.check(&tx);
    world.check(&reader);
    // Neither is entered for the transaction: nothing is written there.
    for name in [OUTER, INNER] {
        let merk = OwnedMerk::change(
            model::subtree(name),
            &BTreeMap::new(),
            &Items::from([(b"n".to_vec(), b"v".to_vec())]),
        );
        let batch = OwnedBatch {
            merks: vec![merk],
            ..OwnedBatch::default()
        };
        let scope = tx.txn.scope().expect("a scope");
        assert_eq!(
            batch_error(batch.with(|batch| scope.apply(batch))),
            BatchError::NotEntered {
                subtree: model::subtree(name)
            }
        );
        scope.merge();
    }

    assert!(matches!(
        world.commit(tx, &[&reader]),
        Outcome::Committed(_)
    ));
    world.check_committed();
    world.check(&reader);
    assert_eq!(planted_read(&world), 0, "no record of either subtree");
    assert_eq!(db.counts.dropped.load(Ordering::Relaxed), 1);
    // One Merk left, and the entry of the Merk below waits for the
    // reclaimer.
    assert_eq!(census(&db), (u64::from(ENTERED), 1, 1));

    // A subtree that is entered again under the dropped prefix starts
    // empty, in every column.
    let tx = world.begin();
    let outer = world.merk(&tx, OUTER, |items| {
        assert!(items.is_empty());
        items.insert(b"host".to_vec(), b"again".to_vec());
    });
    let batch = OwnedBatch {
        merks: vec![outer],
        records: vec![put(aux(OUTER), b"b", b"4")],
        subtrees: vec![enter(OUTER)],
    };
    (world.apply(&tx, &batch)).expect("the subtree is entered again");
    world.check(&tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
    assert_eq!(planted_read(&world), 0, "none of the records of before");
    assert_eq!(census(&db), (u64::from(ENTERED) + 1, 1, 1));
    assert_eq!(db.private_ids(), 0);
}

/// `tree-layout.md` D6, `transactions.md` CV9, T-CL1: a subtree that is
/// entered again under the prefix of one that was below a dropped subtree
/// starts empty in every column, also before the reclaimer has come. The
/// drop reads nothing below the Merk it drops, so the flat records of
/// before are still in their pages: the enter takes them out
/// (`lignin_tree::drop::enter`).
#[test]
fn a_subtree_entered_again_below_a_dropped_one_starts_empty() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let tx = world.begin();
    planted(&world, &tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    let tx = world.begin();
    drop_outer(&world, &tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();

    let tx = world.begin();
    let mut outer = world.merk(&tx, OUTER, |items| {
        items.insert(b"host".to_vec(), b"again".to_vec());
    });
    (outer.hosted).insert(b"host".to_vec(), vec![model::subtree(INNER)]);
    let inner = world.merk(&tx, INNER, |items| {
        assert!(items.is_empty());
        items.insert(b"three".to_vec(), vec![5; 20]);
    });
    let batch = OwnedBatch {
        merks: vec![inner, outer],
        subtrees: vec![enter(OUTER), hosted(INNER, OUTER, b"host")],
        ..OwnedBatch::default()
    };
    (world.apply(&tx, &batch)).expect("the subtrees are entered again");
    // Inside the transaction the records of before are not read: the
    // committed version has no live entry under the prefix.
    world.check(&tx);
    let mut out = Vec::new();
    assert!(!tx.txn.get(aux(INNER), b"ia", &mut out).expect("a read"));
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
    // And none of them is read under the new subtree once it is committed.
    assert_eq!(planted_read(&world), 0, "none of the records of before");
}

/// A drop and what takes its place in one batch, and a clear that keeps
/// the host (`tree-layout.md` D4): the Merk is there, with no node of
/// before.
#[test]
fn a_batch_drops_a_subtree_and_enters_it_again_and_clears_a_merk() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let tx = world.begin();
    planted(&world, &tx);
    world.edit(&tx, 5, |items| {
        for index in 0..40u8 {
            items.insert(vec![b'c', index], vec![index; 60]);
        }
    });
    world.write(&tx, vec![put(model::unit(Column::Data, 6), b"flat", b"f")]);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();

    let tx = world.begin();
    let fresh = |name: u8, items: &[(&[u8], &[u8])]| {
        let items = (items.iter())
            .map(|(key, value)| (key.to_vec(), value.to_vec()))
            .collect();
        OwnedMerk::change(model::subtree(name), &BTreeMap::new(), &items)
    };
    // The drop comes first, wherever it stands in the batch.
    let batch = OwnedBatch {
        merks: vec![fresh(OUTER, &[(b"left", b"new"), (b"only", b"one")])],
        records: vec![put(aux(OUTER), b"z", b"9")],
        subtrees: vec![enter(OUTER), OwnedSubtree::Drop(model::subtree(OUTER))],
    };
    world.apply(&tx, &batch).expect("the subtree is replaced");
    world.check(&tx);

    // A clear, and the Merk's new nodes with it.
    let cleared = OwnedBatch {
        merks: vec![fresh(5, &[(b"c\x03", b"kept"), (b"new", b"node")])],
        subtrees: vec![OwnedSubtree::Clear(model::subtree(5))],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &cleared).expect("the Merk is cleared");
    world.check(&tx);
    // A clear of a Merk with no node leaves nothing, and of a subtree with
    // flat data takes the data.
    for name in [7, 6] {
        let batch = subtrees(vec![OwnedSubtree::Clear(model::subtree(name))]);
        world.apply(&tx, &batch).expect("it is cleared");
    }
    world.check(&tx);
    // The same once the frames were made again from the write set.
    let rebuilds = tx.txn.stats().rebuilds;
    world.edit_outside(
        9,
        |items| drop(items.insert(b"x".to_vec(), b"y".to_vec())),
        &[&tx],
    );
    world.check(&tx);
    assert_eq!(tx.txn.stats().rebuilds, rebuilds + 1);

    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
    assert_eq!(
        db.counts.dropped.load(Ordering::Relaxed),
        2,
        "the dropped Merk and the cleared one that had nodes"
    );
    assert_eq!(census(&db), (u64::from(ENTERED) + 1, 1, 2));
    assert_eq!(db.private_ids(), 0);
}

/// D-079 item 5, CF5: the directory is read again when a commit of another
/// lands, a drop outside meets what the transaction wrote under it, and a
/// transaction that serves its records by key hides what it dropped.
#[test]
fn a_drop_outside_is_seen_by_the_next_read_and_meets_the_writes_under_it() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let tx = world.begin();
    planted(&world, &tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    let inner: [&[u8]; 2] = [b"one", b"two"];

    // One transaction has written beside the subtree, one under it, and
    // one drops it itself after a write that a commit outside will meet.
    let beside = world.begin();
    world.edit(&beside, 2, |items| {
        drop(items.insert(b"k".to_vec(), b"v".to_vec()))
    });
    let under = world.begin();
    world.write(&under, vec![put(aux(OUTER), b"c", b"5")]);
    let own = world.begin();
    world.write(&own, vec![put(aux(1), b"met", b"1")]);
    drop_outer(&world, &own);
    // And one writes the Merk below and nothing of the subtree that will
    // be dropped: its host is not written (never on existing
    // GroveVersions). A node of it keeps its committed value.
    let below = world.begin();
    let mut merk = world.merk(&below, INNER, |items| {
        drop(items.insert(b"three".to_vec(), vec![5; 20]))
    });
    merk.anchored = true;
    assert!(merk.nodes.iter().any(|(.., stated)| !stated));
    let batch = OwnedBatch {
        merks: vec![merk],
        ..OwnedBatch::default()
    };
    world.apply(&below, &batch).expect("a node below");
    assert_eq!(node_count(&beside, INNER, &inner), 2);
    assert_eq!(node_count(&own, INNER, &inner), 0);

    let records = OwnedBatch {
        records: vec![put(aux(1), b"met", b"2")],
        ..OwnedBatch::default()
    };
    world.write_outside(&records, &[&beside, &under, &own]);
    // It has no frames now, and what it dropped stays hidden: by key, and
    // under a cursor over the Merk below.
    world.check(&own);
    assert_eq!(own.txn.stats().base, None);
    assert_eq!(node_count(&own, INNER, &inner), 0);

    let dropper = world.begin();
    drop_outer(&world, &dropper);
    let others = [&beside, &under, &own, &below];
    assert!(matches!(
        world.commit(dropper, &others),
        Outcome::Committed(_)
    ));
    world.check_committed();
    // The next read of the one beside it finds no Merk below the drop, and
    // its own writes are as they were.
    assert_eq!(node_count(&beside, INNER, &inner), 0);
    world.check(&beside);
    let merk = OwnedMerk::change(
        model::subtree(INNER),
        &BTreeMap::new(),
        &Items::from([(b"n".to_vec(), b"v".to_vec())]),
    );
    let batch = OwnedBatch {
        merks: vec![merk],
        ..OwnedBatch::default()
    };
    assert_eq!(
        batch_error(world.apply(&beside, &batch)),
        BatchError::NotEntered {
            subtree: model::subtree(INNER)
        }
    );
    assert!(matches!(world.commit(beside, &[]), Outcome::Committed(_)));
    // The write under the dropped subtree is met.
    world.check(&under);
    assert_eq!(under.txn.stats().overlapped, 1);
    assert_eq!(world.commit(under, &[]), Outcome::Conflict);
    assert_eq!(world.commit(own, &[]), Outcome::Conflict);
    // The one that wrote below is met as well, though the key set of the
    // drop names the dropped subtree alone: the drop deleted every record
    // below it (CF5, CV9). It reads its own nodes, with the values they
    // kept, and nothing committed of the Merk, and its commit fails with
    // the conflict.
    world.check(&below);
    assert!(below.txn.stats().overlapped > 0);
    assert_eq!(node_count(&below, INNER, &[b"one", b"two", b"three"]), 2);
    let held = below.txn.stats();
    assert_eq!((held.pinned, held.base), (None, None));
    assert_eq!(world.commit(below, &[]), Outcome::Conflict);
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
}

/// What the transaction itself wrote below a subtree goes with the drop of
/// that subtree: under a cursor, by key, in the frames and without them,
/// and also where it enters the subtrees again.
#[test]
fn what_the_transaction_wrote_below_a_dropped_subtree_goes_with_it() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let tx = world.begin();
    planted(&world, &tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));

    for by_key in [false, true] {
        let tx = world.begin();
        world.write(&tx, vec![put(aux(1), b"met", &[by_key as u8])]);
        if by_key {
            // A commit outside meets the write: no frames from here on.
            let met = OwnedBatch {
                records: vec![put(aux(1), b"met", b"other")],
                ..OwnedBatch::default()
            };
            world.write_outside(&met, &[&tx]);
        }
        let batch = inner_batch(&world, &tx, |items| {
            drop(items.insert(b"own".to_vec(), vec![6; 30]))
        });
        world.apply(&tx, &batch).expect("a node below");
        world.write(&tx, vec![put(aux(INNER), b"own", b"9")]);
        world.check(&tx);
        assert_eq!(tx.txn.stats().base.is_none(), by_key);

        drop_outer(&world, &tx);
        world.check(&tx);
        assert_eq!(node_count(&tx, INNER, &[b"own", b"one", b"two"]), 0);
        let mut cursor = tx.txn.merk_cursor(model::subtree(INNER)).expect("a cursor");
        cursor.seek_to_first().expect("a seek");
        assert_eq!(cursor.key(), None, "no node it wrote before is shown");
        drop(cursor);
        let mut out = Vec::new();
        assert!(!tx.txn.get(aux(INNER), b"own", &mut out).expect("a read"));

        // Both are entered again, and hold what is written from here on.
        let mut outer = world.merk(&tx, OUTER, |items| {
            assert!(items.is_empty());
            items.insert(b"host".to_vec(), b"again".to_vec());
        });
        (outer.hosted).insert(b"host".to_vec(), vec![model::subtree(INNER)]);
        let inner = world.merk(&tx, INNER, |items| {
            assert!(items.is_empty());
            items.insert(b"new".to_vec(), vec![5; 20]);
        });
        let batch = OwnedBatch {
            merks: vec![inner, outer],
            records: vec![put(aux(INNER), b"fresh", b"1")],
            subtrees: vec![enter(OUTER), hosted(INNER, OUTER, b"host")],
        };
        (world.apply(&tx, &batch)).expect("the subtrees are entered again");
        world.check(&tx);
        assert_eq!(node_count(&tx, INNER, &[b"own", b"one", b"two", b"new"]), 1);
        // And dropped again: what went below is found by the hosts of the
        // subtrees the transaction entered itself.
        drop_outer(&world, &tx);
        world.check(&tx);
        assert_eq!(node_count(&tx, INNER, &[b"own", b"one", b"two", b"new"]), 0);
        assert!(!tx.txn.get(aux(INNER), b"fresh", &mut out).expect("a read"));
        match by_key {
            true => assert_eq!(world.commit(tx, &[]), Outcome::Conflict),
            false => tx.txn.rollback().expect("a rollback"),
        }
    }
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
}

/// The cells of a Merk that leaves stay in their pages, so a handle into
/// them would still name a node: every handle that was read before a drop
/// or a clear ends with it (PN8), in the frames and without them.
#[test]
fn a_drop_ends_the_handles_that_were_read_before_it() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    world.edit_outside(
        5,
        |items| {
            for index in 0..12u8 {
                items.insert(vec![b'k', index], vec![index; 40]);
            }
        },
        &[],
    );
    let key: &[u8] = b"k\x05";
    let subtree = model::subtree(5);
    let handle = |view: Option<NodeView<'_>>| view.and_then(|view| view.handle());
    let hash = [0x11; 32];

    for by_key in [false, true] {
        let tx = world.begin();
        world.write(&tx, vec![put(aux(1), b"met", &[by_key as u8])]);
        if by_key {
            let met = OwnedBatch {
                records: vec![put(aux(1), b"met", b"other")],
                ..OwnedBatch::default()
            };
            world.write_outside(&met, &[&tx]);
        }
        let kept = (tx.txn.node(subtree, key, handle)).expect("a read");
        let kept = kept.expect("a cell");
        let followed = |tx: &Tx<'_>| tx.txn.stats().handles_followed;
        let before = followed(&tx);
        assert!(tx
            .txn
            .follow(subtree, kept, key, |node| node.is_some())
            .expect("a read"));
        assert_eq!(followed(&tx), before + 1);

        // A hint from before the clear is not followed by the batch that
        // clears: the node is new, and has no value to keep.
        let nodes = [NodePut {
            key,
            feature: &[3, 1, 0, 0, 0, 0, 0, 0, 0],
            value_hash: &hash,
            kv_hash: &hash,
            left: None,
            right: None,
            hosted: Hosted::Keep,
            value: ValuePut::Keep,
            stale: lignin_tree::hash::Stale::NONE,
            at: Some(kept),
        }];
        let merks = [MerkWrite {
            subtree,
            nodes: &nodes,
            removed: &[],
            root: RootPut::Set(Some(key)),
            anchored: false,
        }];
        let clear = [SubtreeWrite::Clear { subtree }];
        let scope = tx.txn.scope().expect("a scope");
        let refused = scope.apply(&Batch {
            merks: &merks,
            subtrees: &clear,
            ..Batch::default()
        });
        assert_eq!(batch_error(refused), BatchError::NoValue { subtree });
        scope.merge();

        let batch = subtrees(vec![OwnedSubtree::Clear(subtree)]);
        world.apply(&tx, &batch).expect("the Merk is cleared");
        let before = followed(&tx);
        assert!(tx
            .txn
            .follow(subtree, kept, key, |node| node.is_none())
            .expect("a read"));
        let child = tx
            .txn
            .child(subtree, kept, Side::Left, |node| node.is_some());
        assert_eq!(child.expect("a read"), None, "the handle has ended");
        assert_eq!(followed(&tx), before);
        world.check(&tx);
        tx.txn.rollback().expect("a rollback");
    }
    assert_eq!(db.private_ids(), 0);
}

/// The drop as GroveDB makes it: the element that hosts the Merk is
/// deleted in the batch that drops the Merk. And what a batch may not say
/// of the subtrees it drops and writes.
#[test]
fn a_hosted_merk_is_dropped_with_its_host_and_no_record_is_written_without_its_subtree() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let tx = world.begin();
    planted(&world, &tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    let (outer, inner) = (model::subtree(OUTER), model::subtree(INNER));
    let other = model::subtree(ENTERED + 40);

    let tx = world.begin();
    let refused = |batch: OwnedBatch| world.apply(&tx, &batch).expect_err("it is refused");
    // A drop names the outermost subtree, and takes what is below.
    let both = subtrees(vec![OwnedSubtree::Drop(outer), OwnedSubtree::Drop(inner)]);
    let error = refused(both);
    assert!(
        matches!(error, TxnError::Batch(BatchError::NotEntered { subtree }) if subtree == inner),
        "{error}"
    );
    // A record is of a subtree that is entered, in every column.
    for column in Column::ALL {
        let unit = Unit::new(column, other);
        let batch = OwnedBatch {
            records: vec![put(unit, b"k", b"v")],
            ..OwnedBatch::default()
        };
        let error = refused(batch);
        assert!(
            matches!(error, TxnError::Batch(BatchError::NotEntered { subtree }) if subtree == other),
            "{error}"
        );
    }
    // A Merk with no node hosts none, so one that has a subtree below it
    // is not dropped: the directory would not find what is below.
    let empty = subtrees(vec![
        enter(ENTERED + 40),
        hosted(ENTERED + 41, ENTERED + 40, b"no node"),
    ]);
    world.apply(&tx, &empty).expect("two subtrees with no node");
    let error = refused(subtrees(vec![OwnedSubtree::Drop(other)]));
    assert!(
        matches!(&error, TxnError::Directory(DirectoryError::NotEmpty { prefix }) if prefix == other.as_bytes()),
        "{error}"
    );
    assert_eq!(tx.txn.stats().writes, 0, "none of them added a write");
    tx.txn.rollback().expect("a rollback");
    tx.forget();

    // The host goes, and the Merk with it.
    let tx = world.begin();
    let host = world.merk(&tx, OUTER, |items| drop(items.remove(&b"host"[..])));
    let batch = OwnedBatch {
        merks: vec![host],
        subtrees: vec![OwnedSubtree::Drop(inner)],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &batch).expect("the Merk is dropped");
    world.check(&tx);
    // Nothing is written under it from here on.
    let batch = OwnedBatch {
        records: vec![put(aux(INNER), b"late", b"v")],
        ..OwnedBatch::default()
    };
    let error = world.apply(&tx, &batch).expect_err("it is refused");
    assert!(
        matches!(error, TxnError::Batch(BatchError::NotEntered { subtree }) if subtree == inner),
        "{error}"
    );
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
    assert_eq!(
        planted_read(&world),
        3,
        "the records of the subtree that stays"
    );
    assert_eq!(census(&db), (u64::from(ENTERED) + 1, 0, 1));
    assert_eq!(db.private_ids(), 0);
}

/// A cursor opens its subtree through the directory and asks again after
/// the transaction dropped something: its committed side is not read while
/// the subtree is below a dropped one, and is read again after a rollback.
#[test]
fn a_cursor_reads_no_committed_record_of_a_subtree_that_went_with_a_drop() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let tx = world.begin();
    planted(&world, &tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));

    let tx = world.begin();
    let mut flat = tx.txn.cursor(aux(INNER));
    let mut nodes = tx.txn.merk_cursor(model::subtree(INNER)).expect("a cursor");
    flat.seek_to_first().expect("a seek");
    nodes.seek_to_first().expect("a seek");
    assert_eq!(
        (flat.key(), nodes.key()),
        (Some(&b"ia"[..]), Some(&b"one"[..]))
    );
    drop((flat, nodes));

    let scope = tx.txn.scope().expect("a scope");
    let mut flat = scope.cursor(aux(INNER));
    let mut nodes = scope.merk_cursor(model::subtree(INNER)).expect("a cursor");
    flat.seek_to_first().expect("a seek");
    nodes.seek_to_first().expect("a seek");
    let dropped = subtrees(vec![OwnedSubtree::Drop(model::subtree(OUTER))]);
    (dropped.with(|batch| scope.apply(batch))).expect("the subtree is dropped");
    // The cursors stand on their keys, with nothing under them, and find
    // no other.
    let mut out = Vec::new();
    assert!(!flat.value(&mut out).expect("a read"));
    assert!(nodes.node(|node| node.is_none()).expect("a read"));
    flat.next().expect("a step");
    nodes.next().expect("a step");
    assert_eq!((flat.key(), nodes.key()), (None, None));
    flat.seek_to_last().expect("a seek");
    nodes.seek_for_prev(b"zzz").expect("a seek");
    assert_eq!((flat.key(), nodes.key()), (None, None));
    // A cursor that is opened now is over a subtree the transaction does
    // not have.
    let mut late = scope.cursor(model::unit(Column::Meta, INNER));
    late.seek(b"").expect("a seek");
    assert_eq!(late.key(), None);
    drop((flat, nodes, late));
    scope.rollback();

    // After the rollback the same subtree is read again.
    let mut flat = tx.txn.cursor(aux(INNER));
    let mut nodes = tx.txn.merk_cursor(model::subtree(INNER)).expect("a cursor");
    flat.seek_to_last().expect("a seek");
    nodes.seek_to_last().expect("a seek");
    assert_eq!(
        (flat.key(), nodes.key()),
        (Some(&b"ia"[..]), Some(&b"two"[..]))
    );
    drop((flat, nodes));
    world.check(&tx);

    // A clear of the cursor's own subtree, and its rollback: the cursor
    // goes on over the committed records from the key it stands on.
    let data = model::unit(Column::Data, 6);
    world.write(&tx, vec![put(data, b"a", b"1"), put(data, b"c", b"3")]);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    let tx = world.begin();
    let mut cursor = tx.txn.cursor(data);
    cursor.seek_to_first().expect("a seek");
    assert_eq!(cursor.key(), Some(&b"a"[..]));
    let mark = tx.txn.mark();
    let cleared = OwnedBatch {
        records: vec![put(data, b"b", b"2"), put(data, b"d", b"4")],
        subtrees: vec![OwnedSubtree::Clear(model::subtree(6))],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &cleared).expect("the data is cleared");
    cursor.next().expect("a step");
    assert_eq!(cursor.key(), Some(&b"b"[..]));
    cursor.next().expect("a step");
    assert_eq!(
        cursor.key(),
        Some(&b"d"[..]),
        "the committed record is gone"
    );
    assert!(tx.txn.rewind(mark));
    tx.forget();
    cursor.prev().expect("a step");
    assert_eq!(cursor.key(), Some(&b"c"[..]));
    cursor.prev().expect("a step");
    assert_eq!(cursor.key(), Some(&b"a"[..]));
    cursor.next().expect("a step");
    assert_eq!(cursor.key(), Some(&b"c"[..]));
    cursor.next().expect("a step");
    assert_eq!(cursor.key(), None);
    drop(cursor);
    world.check(&tx);
    assert_eq!(db.private_ids(), 0);
}

/// An enter that is taken back leaves the subtree below nothing: entered
/// again as a subtree that stands alone, it stays when what would have
/// hosted it is dropped.
#[test]
fn an_enter_that_was_taken_back_puts_the_subtree_below_nothing() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let tx = world.begin();
    let outer = world.merk(&tx, OUTER, |items| {
        drop(items.insert(b"host".to_vec(), b"tree".to_vec()))
    });
    let batch = OwnedBatch {
        merks: vec![outer],
        subtrees: vec![enter(OUTER)],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &batch).expect("the subtree is entered");
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));

    let tx = world.begin();
    let (mark, writes) = (tx.txn.mark(), tx.model.borrow().writes.len());
    let below = OwnedBatch {
        records: vec![put(aux(INNER), b"below", b"1")],
        subtrees: vec![hosted(INNER, OUTER, b"host")],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &below).expect("it is entered below");
    assert!(tx.txn.rewind(mark));
    tx.model.borrow_mut().writes.truncate(writes);
    world.check(&tx);
    // It is entered again, with no host, and what was to host it goes.
    let alone = OwnedBatch {
        records: vec![put(aux(INNER), b"alone", b"2")],
        subtrees: vec![enter(INNER)],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &alone).expect("it is entered alone");
    drop_outer(&world, &tx);
    world.check(&tx);
    let mut out = Vec::new();
    assert!(tx.txn.get(aux(INNER), b"alone", &mut out).expect("a read"));
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
    assert_eq!(census(&db), (u64::from(ENTERED) + 1, 0, 1));
    assert_eq!(db.private_ids(), 0);
}

fn cell(page: u32, slot: u16) -> CellAddress {
    let page = PageId::new(page).expect("a page id");
    CellAddress::new(page, SlotId::new(slot).expect("a slot"))
}

/// A defect of the first unit: the roots that one write changed were told
/// to the directory one after the other, by the address each had. Where a
/// new root came to the cell that a carried root had left in the same
/// write, two entries held one address for a moment, and the wrong Merk
/// got the carried root's new place.
///
/// The changes of one write are told in one statement, which is resolved
/// against the entries as they were before it: here for a new root in the
/// cell a carried root left, for a carried root in the cell another carried
/// root left, and for a kept root that the report names as well.
#[test]
fn the_roots_of_one_write_are_told_together() {
    let (db, versions) = Db::new(64);
    let guard = versions.guard();
    let base = db.committed(&guard, versions.latest());
    let mut pages = Private::new(base, &db.pool, OwnerId::new(u64::MAX - 1), &db.ids);
    let directory = db.directory_at(&pages);
    let prefix = |name: u8| *model::subtree(name).as_bytes();
    let root_of = |pages: &Private<'_, '_, _, _>, name: u8| {
        let entry = directory.open(pages, &prefix(name)).expect("a lookup");
        let entry = entry.expect("the Merk");
        let root = entry.root();
        root.map(|root| (root.address, root.key.to_vec()))
    };
    let named = |name: u8, root: RootPut<'static>| MerkWrite {
        subtree: model::subtree(name),
        nodes: &[],
        removed: &[],
        root,
        anchored: false,
    };

    // Three Merks with their roots in one page.
    let (first, second, third) = (cell(6, 17), cell(6, 18), cell(6, 19));
    let start = [(61, first, b"a"), (62, second, b"b"), (63, third, b"c")];
    let made: Vec<([u8; 32], Root<'_>)> = (start.iter())
        .map(|(name, address, key)| {
            let address = *address;
            (prefix(*name), Root { address, key: *key })
        })
        .collect();
    let changes: Vec<RootChange<'_>> = (made.iter())
        .map(|(prefix, root)| RootChange::Named {
            prefix,
            root: Some(*root),
        })
        .collect();
    (directory.set_roots(&mut pages, &changes)).expect("the roots are set");

    // A new root in the cell that a carried root left: the Merk 98 gets
    // its first node where the root of 61 was, which went to another page.
    let away = cell(33, 14);
    let write = named(98, RootPut::Set(Some(b"new")));
    let hosted = [RootChange::Carried {
        from: first,
        to: away,
    }];
    let changes = root_changes(
        [(&write, None, Some(first))].into_iter(),
        hosted.into_iter(),
    );
    (directory.set_roots(&mut pages, &changes)).expect("the roots are told");
    assert_eq!(root_of(&pages, 98), Some((first, b"new".to_vec())));
    assert_eq!(root_of(&pages, 61), Some((away, b"a".to_vec())));
    let owner = |pages: &Private<'_, '_, _, _>, address| {
        (directory.root_owner(pages, address)).expect("a lookup")
    };
    assert_eq!(owner(&pages, first), Some(prefix(98)));
    assert_eq!(owner(&pages, away), Some(prefix(61)));

    // The chain: one carried root comes to the cell another carried root
    // left, and a third to the cell the second left. The last of them is a
    // Merk of the batch that keeps its root, which the report names too.
    let (far, farther) = (cell(40, 1), cell(41, 1));
    let kept = named(63, RootPut::Keep);
    let hosted = [
        RootChange::Carried {
            from: second,
            to: third,
        },
        RootChange::Carried {
            from: third,
            to: far,
        },
        RootChange::Carried {
            from: away,
            to: second,
        },
    ];
    let untouched = named(1, RootPut::Keep);
    let merks = [(&kept, Some(third), Some(far)), (&untouched, None, None)];
    let changes = root_changes(merks.into_iter(), hosted.into_iter());
    assert_eq!(changes.len(), 3, "a root is told once");
    (directory.set_roots(&mut pages, &changes)).expect("the roots are told");
    assert_eq!(root_of(&pages, 62), Some((third, b"b".to_vec())));
    assert_eq!(root_of(&pages, 63), Some((far, b"c".to_vec())));
    assert_eq!(root_of(&pages, 61), Some((second, b"a".to_vec())));
    assert_eq!(owner(&pages, second), Some(prefix(61)));
    assert_eq!(owner(&pages, third), Some(prefix(62)));
    assert_eq!(owner(&pages, away), None);

    // A Merk that loses its last node, one that had none and has none, and
    // a root that stays where it is.
    let emptied = named(98, RootPut::Set(None));
    let empty = named(2, RootPut::Set(None));
    let same = named(62, RootPut::Set(Some(b"b")));
    let merks = [
        (&emptied, Some(first), None),
        (&empty, None, None),
        (&same, Some(third), Some(third)),
        (&kept, Some(far), Some(farther)),
    ];
    let changes = root_changes(merks.into_iter(), std::iter::empty());
    assert_eq!(changes.len(), 3);
    (directory.set_roots(&mut pages, &changes)).expect("the roots are told");
    assert_eq!(root_of(&pages, 98), None);
    assert_eq!(root_of(&pages, 62), Some((third, b"b".to_vec())));
    assert_eq!(root_of(&pages, 63), Some((farther, b"c".to_vec())));
    let census = directory.check(&pages).expect("the directory is sound");
    assert_eq!(census.live, u64::from(ENTERED));

    // Two Merks in one cell are refused, with nothing written.
    let clash = named(2, RootPut::Set(Some(b"x")));
    let changes = root_changes(
        [(&clash, None, Some(third))].into_iter(),
        std::iter::empty(),
    );
    let copies = pages.written().count();
    let refused = directory.set_roots(&mut pages, &changes);
    assert!(
        matches!(refused, Err(DirectoryError::RootTaken { address }) if address == third),
        "{refused:?}"
    );
    assert_eq!(pages.written().count(), copies, "no page was copied for it");
    assert_eq!(root_of(&pages, 2), None);
    assert_eq!(root_of(&pages, 62), Some((third, b"b".to_vec())));
    assert_eq!(owner(&pages, third), Some(prefix(62)));
    let again = directory.check(&pages).expect("the directory is sound");
    assert_eq!(again, census, "the directory is as it was");
    pages.roll_back().expect("the copies are given up");
}

/// The Merk at the root of the grove of the test below.
const ROOT: u8 = 50;

fn tree(index: usize) -> u8 {
    60 + index as u8
}

fn element(index: usize) -> Vec<u8> {
    format!("tree-{index:02}").into_bytes()
}

fn key(index: usize) -> Vec<u8> {
    format!("n{index:04}").into_bytes()
}

/// A batch that changes one tree of the grove by `edit`, and the element
/// that hosts it in the root Merk, which names the tree's root again.
fn grove<'s>(
    world: &World<'s>,
    tx: &Tx<'s>,
    index: usize,
    edit: &mut dyn FnMut(&mut Items),
) -> OwnedBatch {
    let mut merks = Vec::new();
    let merk = world.merk(tx, tree(index), |items| edit(items));
    let changed = !merk.nodes.is_empty() || !merk.removed.is_empty() || merk.root.is_some();
    let stamp = merk.nodes.len() as u8;
    if changed {
        merks.push(merk);
    }
    let mut root = world.merk(tx, ROOT, |items| {
        let mut value = b"tree:".to_vec();
        value.extend_from_slice(&[stamp, items.len() as u8]);
        value.extend_from_slice(&items.get(&element(index)).cloned().unwrap_or_default());
        value.truncate(60);
        items.insert(element(index), value);
    });
    (root.hosted).insert(element(index), vec![model::subtree(tree(index))]);
    let records = World::root_record(&root).into_iter().collect();
    merks.push(root);
    OwnedBatch {
        merks,
        records,
        ..OwnedBatch::default()
    }
}

/// The same defect through the writes of a transaction: a grove of trees
/// whose elements host their Merks is written until a write gives a Merk
/// its root in a cell that the root of another Merk, carried with its host,
/// has just left. Every node of every Merk is read after every write.
#[test]
fn a_new_root_in_the_cell_a_carried_root_left_keeps_both_merks_whole() {
    let mut seen = (0, 0);
    for seed in 400..440 {
        let (db, versions) = Db::new(256);
        let mut world = World::new(&db, &versions);
        let mut rng = Rng::new(seed);
        let tx = world.begin();
        for index in 0..10 {
            let batch = grove(&world, &tx, index, &mut |items| {
                for node in 0..3 + index * 6 {
                    let len = 10 + rng.below(240);
                    items.insert(key(node * 3), value(&mut rng, len));
                }
            });
            world.apply(&tx, &batch).expect("the grove is made");
        }
        assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
        world.check_committed();

        let tx = world.begin();
        for level in 0..8 {
            let batch = grove(&world, &tx, level % 10, &mut |items| {
                for node in 0..40 {
                    let len = match node % 4 {
                        0 => 260 + rng.below(400),
                        _ => 20 + rng.below(100),
                    };
                    items.insert(key(2_000 + level * 50 + node), value(&mut rng, len));
                }
                let first = items.keys().next().cloned().expect("a node");
                items.remove(&first);
            });
            world.apply(&tx, &batch).expect("a tree grows");
            world.check(&tx);
            let other = match level % 2 {
                0 => grove(&world, &tx, 20 + level, &mut |items| {
                    items.insert(key(level), value(&mut rng, 80));
                }),
                _ => grove(&world, &tx, (level + 5) % 10, &mut Items::clear),
            };
            world.apply(&tx, &other).expect("a tree is made or emptied");
            world.check(&tx);
        }
        assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
        world.check_committed();
        assert_eq!(db.private_ids(), 0);
        seen = probe::counts();
        if seen.0 > 0 {
            break;
        }
    }
    assert!(
        seen.0 > 0,
        "no write gave a Merk its root in the cell a carried root left"
    );
}
