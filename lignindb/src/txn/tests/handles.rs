//! Locations that are kept between operations (`transactions.md` PN8,
//! T-RD6): a node handle is followed while it is good, and one that was
//! kept across a commit outside the transaction, a rebase, a rollback, a
//! rollback to a mark or a scope rollback is not followed. The read then
//! returns the record the read rule names, found by its key, also when the
//! page the handle names was freed and its id given to another page.

use std::{
    path::Path,
    sync::{atomic::Ordering, Arc},
};

use lignin_format::tree_page::Side;
use lignin_io::{
    sim::{Fault, FaultRule, OpKind, SimConfig, SimVfs},
    Vfs,
};
use lignin_store::{flush::FlushConfig, wal::LogOptions};
use lignin_tree::node::NodeHandle;

use super::{
    model::{self, records, value, NodeRec, OwnedBatch},
    store::{Db, Outcome, Rng, SIZE},
    *,
};

type Kept = Vec<(Vec<u8>, NodeHandle)>;

fn key(index: usize) -> Vec<u8> {
    format!("n{index:04}").into_bytes()
}

/// The node records of the Merk `name` as `tx` reads them.
fn view(world: &World<'_>, tx: &Tx<'_>, name: u8) -> BTreeMap<Vec<u8>, NodeRec> {
    let unit = Unit::data(model::subtree(name));
    (tx.model
        .borrow()
        .view(&world.model.records, unit)
        .into_iter())
    .map(|(key, value)| match value {
        Val::Node(node) => (key, node),
        Val::Flat(_) => unreachable!("a Merk"),
    })
    .collect()
}

/// Reads every node of the Merk `name` by its key and keeps its handle.
fn keep(world: &World<'_>, tx: &Tx<'_>, name: u8) -> Kept {
    (view(world, tx, name).into_iter())
        .map(|(key, node)| {
            let read = |view: Option<NodeView<'_>>| {
                let view = view.expect("the node is there");
                node.check(&key, &view);
                view.handle().expect("a cell has a handle")
            };
            let handle = (tx.txn.node(model::subtree(name), &key, read)).expect("a read");
            (key, handle)
        })
        .collect()
}

/// Reads every kept node through its handle, checks that each read gives
/// what the read rule names for its key now, and returns how many of the
/// handles were followed.
fn follow(world: &World<'_>, tx: &Tx<'_>, name: u8, kept: &Kept) -> u64 {
    let expected = view(world, tx, name);
    let before = tx.txn.stats().handles_followed;
    for (key, handle) in kept {
        let read = |view: Option<NodeView<'_>>| match (expected.get(key), view) {
            (None, None) => {}
            (Some(node), Some(view)) => node.check(key, &view),
            (expected, view) => panic!("{key:?}: {expected:?} is expected, {view:?} is shown"),
        };
        (tx.txn.follow(model::subtree(name), *handle, key, read)).expect("a read");
    }
    tx.txn.stats().handles_followed - before
}

/// Walks the Merk `name` from its root by page pointers: the child of each
/// node through the node's handle. Returns how many nodes it reached.
fn descend(world: &World<'_>, tx: &Tx<'_>, name: u8) -> usize {
    let expected = view(world, tx, name);
    let items = expected
        .iter()
        .map(|(key, node)| (key.clone(), node.value.clone()));
    let Some(root) = records(&items.collect()).0 else {
        return 0;
    };
    let subtree = model::subtree(name);
    let handle_of = |view: Option<NodeView<'_>>| view.and_then(|view| view.handle());
    let root_handle = (tx.txn.node(subtree, &root, handle_of)).expect("a read");
    let mut stack = vec![(root, root_handle.expect("the root is a cell"))];
    let mut reached = 0;
    while let Some((at, handle)) = stack.pop() {
        reached += 1;
        for side in [Side::Left, Side::Right] {
            let link = expected[&at].link(side);
            let read = |view: Option<NodeView<'_>>| {
                view.map(|view| {
                    expected[view.key()].check(view.key(), &view);
                    (view.key().to_vec(), view.handle().expect("a cell"))
                })
            };
            let child = (tx.txn.child(subtree, handle, side, read)).expect("a read");
            let child = child.expect("the parent's handle is good");
            assert_eq!(
                child.as_ref().map(|(key, _)| key),
                link.map(|link| &link.key),
                "the child of {at:?}"
            );
            stack.extend(child);
        }
    }
    assert_eq!(
        reached,
        expected.len(),
        "every node is reached from the root"
    );
    reached
}

/// Reads both children of every kept node of the Merk `name` through the
/// node's handle. Returns how many of the reads were answered, with a
/// child or with none, how many said that the handle is no longer good,
/// and how many failed.
fn children(tx: &Tx<'_>, name: u8, kept: &Kept) -> (usize, usize, usize) {
    let (mut answered, mut ended, mut failed) = (0, 0, 0);
    for (_, handle) in kept {
        for side in [Side::Left, Side::Right] {
            match (tx.txn).child(model::subtree(name), *handle, side, |_| ()) {
                Ok(Some(())) => answered += 1,
                Ok(None) => ended += 1,
                Err(_) => failed += 1,
            }
        }
    }
    (answered, ended, failed)
}

fn fill(world: &mut World<'_>, rng: &mut Rng, name: u8, from: usize, count: usize) {
    world.edit_outside(
        name,
        |items| {
            for index in from..from + count {
                let len = 60 + rng.below(120);
                items.insert(key(index), value(rng, len));
            }
        },
        &[],
    );
}

#[test]
fn a_handle_is_followed_while_it_is_good_and_not_after_its_lease_ended() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(31);
    fill(&mut world, &mut rng, 1, 0, 90);
    fill(&mut world, &mut rng, 2, 0, 40);

    // A transaction that has written nothing reads at the latest version,
    // and its handles are good while that does not move.
    let tx = world.begin();
    let kept = keep(&world, &tx, 1);
    assert_eq!(
        follow(&world, &tx, 1, &kept),
        90,
        "every handle is followed"
    );
    assert_eq!(descend(&world, &tx, 1), 90);
    world.edit_outside(2, |items| drop(items.remove(&key(3))), &[&tx]);
    assert_eq!(follow(&world, &tx, 1, &kept), 0, "a commit was published");
    let kept = keep(&world, &tx, 1);
    assert_eq!(follow(&world, &tx, 1, &kept), 90);

    // Its own write keeps the handles into the pages that no cell left.
    world.edit(&tx, 1, |items| {
        items.insert(key(1000), b"a new node".to_vec());
    });
    let kept = keep(&world, &tx, 1);
    assert_eq!(follow(&world, &tx, 1, &kept), 91);
    assert_eq!(descend(&world, &tx, 1), 91);
    world.edit(&tx, 1, |items| {
        for index in 0..30 {
            items.remove(&key(index * 3));
        }
    });
    let followed = follow(&world, &tx, 1, &kept);
    assert!(followed < 91, "cells left their pages: {followed} followed");
    assert_eq!(descend(&world, &tx, 1), 61);

    // A commit outside that the transaction is rebased over ends them all.
    let kept = keep(&world, &tx, 1);
    world.edit_outside(2, |items| drop(items.remove(&key(5))), &[&tx]);
    assert_eq!(
        follow(&world, &tx, 1, &kept),
        0,
        "the frames were made again"
    );
    assert_eq!(tx.txn.stats().rebuilds, 1);
    let kept = keep(&world, &tx, 1);
    assert_eq!(follow(&world, &tx, 1, &kept), 61);

    // A rollback to a mark ends them, also those of the nodes it did not
    // touch.
    let mark = tx.txn.mark();
    let writes = tx.model.borrow().writes.len();
    world.edit(&tx, 1, |items| {
        items.insert(key(2000), b"to be rolled back".to_vec());
    });
    let kept = keep(&world, &tx, 1);
    tx.txn.rewind(mark);
    tx.model.borrow_mut().writes.truncate(writes);
    assert_eq!(follow(&world, &tx, 1, &kept), 0, "the frames went back");

    // A scope that is rolled back ends them.
    let scope = tx.txn.scope().expect("no scope is open");
    let merk = world.merk(&tx, 1, |items| {
        items.insert(key(3000), b"in a scope".to_vec());
    });
    let records = World::root_record(&merk).into_iter().collect();
    let merks = vec![merk];
    (OwnedBatch {
        merks,
        records,
        ..OwnedBatch::default()
    }
    .with(|batch| scope.apply(batch)))
    .expect("a write");
    let handle = |view: Option<NodeView<'_>>| view.and_then(|view| view.handle());
    let inside = (scope.node(model::subtree(1), &key(3000), handle)).expect("a read");
    let inside = inside.expect("the scope reads its own write");
    let other = (scope.node(model::subtree(1), &key(1), handle)).expect("a read");
    scope.rollback();
    let kept = vec![(key(3000), inside), (key(1), other.expect("a node"))];
    assert_eq!(follow(&world, &tx, 1, &kept), 0);

    // A rollback of the whole transaction ends them.
    let kept = keep(&world, &tx, 1);
    tx.txn.rollback().expect("no scope is open");
    tx.forget();
    assert_eq!(follow(&world, &tx, 1, &kept), 0);
    assert_eq!(descend(&world, &tx, 1), 90);
}

/// A handle that is kept while the page it names is freed and its id is
/// given to another page never leads to a node of that page.
#[test]
fn a_kept_handle_never_leads_into_a_page_that_took_its_page_id() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(32);
    fill(&mut world, &mut rng, 1, 0, 120);
    let reused = || db.counts.ids_reused.load(Ordering::Relaxed);

    let idle = world.begin();
    let writer = world.begin();
    world.write(&writer, vec![put(model::unit(Column::Aux, 0), b"k", b"v")]);
    let mut kept_idle = keep(&world, &idle, 1);
    let mut kept_writer = keep(&world, &writer, 1);
    let mut stale = 0;
    for round in 0..8 {
        let before = reused();
        // The Merk is emptied and filled with other nodes, under some of
        // the same keys, and a second Merk takes pages in between.
        world.edit_outside(1, |items| items.clear(), &[&idle, &writer]);
        fill(&mut world, &mut rng, 2, round * 100, 60);
        world.edit_outside(
            1,
            |items| {
                for index in 0..120 {
                    if rng.chance(70) {
                        items.insert(key(index), value(&mut rng, 100));
                    }
                }
            },
            &[&idle, &writer],
        );
        world.edit_outside(2, |items| items.clear(), &[&idle, &writer]);
        if round > 1 {
            assert!(reused() > before, "round {round}: page ids were used again");
        }
        // The handles are from before all that. None is followed, and
        // every read gives the node that is under the key now.
        stale += kept_idle.len() + kept_writer.len();
        assert_eq!(follow(&world, &idle, 1, &kept_idle), 0);
        assert_eq!(follow(&world, &writer, 1, &kept_writer), 0);
        kept_idle = keep(&world, &idle, 1);
        kept_writer = keep(&world, &writer, 1);
        assert_eq!(follow(&world, &idle, 1, &kept_idle), kept_idle.len() as u64);
        assert_eq!(descend(&world, &writer, 1), kept_writer.len());
    }
    assert!(
        stale > 1_000 && reused() > 20,
        "{stale} handles, {} ids",
        reused()
    );
    assert!(matches!(
        world.commit(writer, &[&idle]),
        Outcome::Committed(_)
    ));
    world.check_committed();
}

/// Where the transaction serves its records by key (WT7b), a handle leads
/// to the committed node only while the transaction has not written that
/// key, and the child of a node is the transaction's own node under the
/// child's key if it wrote one (RD5).
#[test]
fn an_overlapped_transaction_follows_handles_to_what_it_has_not_written() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(33);
    fill(&mut world, &mut rng, 1, 0, 40);
    let subtree = model::subtree(1);
    let tx = world.begin();
    world.edit(&tx, 1, |items| {
        items.insert(key(7), b"the transaction's".to_vec());
    });
    // A commit outside writes the same Merk: the transaction is overlapped.
    // Its new nodes come to stand above nodes the transaction wrote.
    world.edit_outside(
        1,
        |items| {
            for index in 0..40 {
                let mut between = key(index);
                between.push(b'x');
                items.insert(between, b"outside".to_vec());
            }
        },
        &[&tx],
    );
    world.check(&tx);
    assert!(tx.txn.stats().overlapped > 0);
    let expected = view(&world, &tx, 1);

    // Each node by its key: a record of the write set has no handle, and a
    // committed node has one that is followed.
    let mut kept = Kept::new();
    for (at, node) in &expected {
        let read = |view: Option<NodeView<'_>>| {
            let view = view.expect("the node is there");
            node.check(at, &view);
            (view.is_record(), view.handle())
        };
        let (record, handle) = (tx.txn.node(subtree, at, read)).expect("a read");
        assert_eq!(record, handle.is_none());
        kept.extend(handle.map(|handle| (at.clone(), handle)));
    }
    assert!(!kept.is_empty() && kept.len() < expected.len());
    assert_eq!(follow(&world, &tx, 1, &kept), kept.len() as u64);

    // The children of the committed nodes, through their handles: each is
    // what the read rule names for the child's key.
    let mut own = 0;
    for (at, handle) in &kept {
        for side in [Side::Left, Side::Right] {
            let read = |view: Option<NodeView<'_>>| {
                view.map(|view| {
                    expected[view.key()].check(view.key(), &view);
                    view.is_record()
                })
            };
            let child = (tx.txn.child(subtree, *handle, side, read)).expect("a read");
            let child = child.unwrap_or_else(|| panic!("the handle of {at:?} is good"));
            own += usize::from(child == Some(true));
        }
    }
    assert!(
        own > 0,
        "a committed node links a node the transaction wrote"
    );

    // Once the transaction writes a key, the handle of the committed node
    // under it is not followed any more.
    let (written, handle) = kept[0].clone();
    world.edit(&tx, 1, |items| {
        drop(items.insert(written.clone(), b"now mine".to_vec()))
    });
    let expected = view(&world, &tx, 1);
    let read = |view: Option<NodeView<'_>>| {
        let view = view.expect("the node is there");
        expected[&written].check(&written, &view);
        view.is_record()
    };
    assert!((tx.txn.follow(subtree, handle, &written, read)).expect("a read"));
    let stale = (tx.txn.child(subtree, handle, Side::Left, |_| ())).expect("a read");
    assert_eq!(stale, None, "the transaction's record is read by its key");
    world.check(&tx);
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);
}

/// A page that the transaction's own writes freed ends the handles into
/// it (PN8): the read of a child through one says that the handle is no
/// longer good, as where the cell left its page, and is no failed read.
#[test]
fn a_handle_into_a_page_that_its_transaction_freed_is_no_longer_good() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(47);
    fill(&mut world, &mut rng, 1, 0, 90);
    fill(&mut world, &mut rng, 2, 0, 10);

    // The transaction has written, so the handles are of its frames.
    let tx = world.begin();
    world.edit(&tx, 2, |items| {
        items.insert(key(1000), b"a new node".to_vec());
    });
    let kept = keep(&world, &tx, 1);
    assert_eq!(children(&tx, 1, &kept), (180, 0, 0));

    // Every node of the Merk leaves, and the pages that held nothing else
    // are freed with them.
    world.edit(&tx, 1, |items| items.clear());
    assert_eq!(children(&tx, 1, &kept), (0, 180, 0));
    world.check(&tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
}

/// A read of the page of the parent's cell that fails is the error of the
/// read of a child through the parent's handle (PN8): the handle has not
/// ended, and the same read is answered once the page can be read. So it
/// is for a transaction that has written nothing, which reads committed
/// pages, and for one that has frames of its own over them.
#[test]
fn a_failed_read_of_the_parents_page_is_the_error_of_the_read_of_a_child() {
    for written in [false, true] {
        let fs = SimVfs::new(SimConfig::new(53));
        fs.create_dir(Path::new("/db")).expect("a directory");
        let vfs: Arc<dyn Vfs> = Arc::new(fs.clone());
        let (options, flush) = (LogOptions::default(), FlushConfig::new(SIZE));
        let (db, versions) = Db::on(vfs, Path::new("/db"), 24, options, flush, None);
        let mut world = World::new(&db, &versions);
        let mut rng = Rng::new(53);
        fill(&mut world, &mut rng, 1, 0, 90);
        for round in 0..8 {
            fill(&mut world, &mut rng, 2, round * 100, 100);
        }
        // The pages are in the segments, so the pool can let them go.
        let write_back = db.write_back(&versions);
        while write_back.pass().expect("a pass").progressed() {}

        let tx = world.begin();
        if written {
            world.edit(&tx, 2, |items| {
                items.insert(key(5000), b"a new node".to_vec());
            });
            assert!(tx.txn.stats().base.is_some(), "it has frames");
        }
        // The reads of the other Merk push the pages of the first out of
        // the pool, and no read of a file is answered from then on.
        let kept = keep(&world, &tx, 1);
        drop(keep(&world, &tx, 2));
        let missed = db.pool.stats().misses;
        fs.add_rule(FaultRule::new(Fault::Eio).on(OpKind::Read).forever());
        let failing = children(&tx, 1, &kept);
        fs.clear_rules();
        let (_, ended, failed) = failing;
        assert_eq!(ended, 0, "no handle has ended: {failing:?}, {written}");
        assert!(failed > 0, "no page was read from its file: {written}");
        assert_eq!(fs.stats().faults, failed as u64, "{written}");
        assert!(db.pool.stats().misses >= missed + failed as u64);

        assert_eq!(children(&tx, 1, &kept), (180, 0, 0), "{written}");
        world.check(&tx);
        match written {
            true => assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_))),
            false => drop(tx),
        }
        world.check_committed();
    }
}
