//! Subtrees that a batch enters, drops and clears, and what the subtree
//! directory is told of the roots a write changed (`lignin_tree::directory`,
//! `transactions.md` CV9, CF5, `tree-layout.md` S3, D1, D4).

use std::sync::atomic::Ordering;

use lignin_format::{CellAddress, PageId, SlotId};
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
/// them, and `INNER` under its node `host`, with two nodes.
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
        ],
        subtrees: vec![enter(OUTER), hosted(INNER, OUTER, b"host")],
    };
    world.apply(tx, &batch).expect("the subtrees are made");
}

/// Drops `OUTER` through `tx`. The model is told of the Merk below it as
/// well, which the drop does not name.
fn drop_outer<'s>(world: &World<'s>, tx: &Tx<'s>) {
    let first_write = world.latest().get();
    let batch = subtrees(vec![OwnedSubtree::Drop(model::subtree(OUTER))]);
    world.apply(tx, &batch).expect("the subtree is dropped");
    let below = model::Write::Drop(model::subtree(INNER));
    (tx.model.borrow_mut().writes).push((below, first_write, false));
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
    assert_eq!(db.counts.dropped.load(Ordering::Relaxed), 1);
    // One Merk left, and the entry of the Merk below waits for the
    // reclaimer.
    assert_eq!(census(&db), (u64::from(ENTERED), 1, 1));

    // A subtree that is entered again under a dropped prefix starts empty,
    // and so does one under the prefix of what was below.
    let tx = world.begin();
    let outer = world.merk(&tx, OUTER, |items| {
        assert!(items.is_empty());
        items.insert(b"host".to_vec(), b"again".to_vec());
    });
    let inner = world.merk(&tx, INNER, |items| {
        assert!(items.is_empty());
        items.insert(b"three".to_vec(), vec![5; 20]);
    });
    let batch = OwnedBatch {
        merks: vec![inner, outer],
        records: vec![put(aux(OUTER), b"b", b"4")],
        subtrees: vec![enter(OUTER), hosted(INNER, OUTER, b"host")],
    };
    world
        .apply(&tx, &batch)
        .expect("the subtrees are entered again");
    world.check(&tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
    assert_eq!(census(&db), (u64::from(ENTERED) + 2, 0, 1));
    assert_eq!(db.private_ids(), 0);
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
    let first_write = world.latest().get();
    world.apply(&tx, &batch).expect("the subtree is replaced");
    let below = model::Write::Drop(model::subtree(INNER));
    let mut writes = tx.model.borrow().writes.clone();
    let at = writes.len() - batch.writes().len() + 1;
    writes.insert(at, (below, first_write, false));
    tx.model.borrow_mut().writes = writes;
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
    let others = [&beside, &under, &own];
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
    world.check_committed();
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
    let refused = directory.set_roots(&mut pages, &changes);
    assert!(
        matches!(refused, Err(DirectoryError::RootTaken { address }) if address == third),
        "{refused:?}"
    );
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
