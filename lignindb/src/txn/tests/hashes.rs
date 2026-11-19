//! Stale hashes in a write transaction (`lignin_tree::hash`,
//! `transactions.md` CM3 step 5): the marks are a part of the frames, they
//! follow the cells and are made again with the frames, and the commit
//! evaluates them. Where the writer makes every hash itself, as on
//! existing GroveVersions, nothing is marked and no rule is called.

use std::sync::atomic::Ordering;

use lignin_format::tree_page::{Hash, HashField};
use lignin_tree::hash::Stale;

use super::{
    model::{self, value, OwnedBatch, UNMADE},
    store::{Db, Outcome, Rng},
    *,
};

#[test]
fn no_rule_is_called_where_the_writer_makes_every_hash() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(61);
    // What an existing GroveVersion does: every node is written with its
    // hashes. Pages are cut, the transaction is rebased, and it commits.
    for round in 0..5u8 {
        let tx = world.begin();
        world.edit(&tx, 1, |items| {
            for index in 0..40u8 {
                items.insert(vec![b'k', round, index], value(&mut rng, 120));
            }
        });
        world.edit_outside(
            2,
            |items| drop(items.insert(vec![round], value(&mut rng, 60))),
            &[&tx],
        );
        world.check(&tx);
        assert_eq!(tx.txn.stats().stale_nodes, 0);
        assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    }
    world.check_committed();
    let count = |counter: &std::sync::atomic::AtomicU64| counter.load(Ordering::Relaxed);
    assert_eq!(count(&db.counts.commits), 10);
    assert_eq!(count(&db.counts.stale_nodes), 0, "nothing was marked");
    assert_eq!(count(&db.counts.rule_calls), 0, "no rule was called");
}

/// What the transaction shows of the node under `key`: its stale hashes,
/// and its value hash as it is stored.
fn shown(tx: &Tx<'_>, key: &[u8]) -> (Stale, Hash) {
    let read = |view: Option<NodeView<'_>>| {
        let view = view.expect("a node");
        (view.stale(), *view.value_hash())
    };
    (tx.txn.node(model::subtree(1), key, read)).expect("a read")
}

#[test]
fn stale_hashes_follow_their_cells_and_are_made_by_the_commit() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(62);
    world.edit_outside(
        1,
        |items| {
            for index in 0..30u8 {
                items.insert(vec![b'a', index], value(&mut rng, 60));
            }
        },
        &[],
    );
    let rule_calls = || db.counts.rule_calls.load(Ordering::Relaxed);
    assert_eq!(rule_calls(), 0);

    // The transaction adds nodes whose value hash and key-value hash it
    // does not make: it states any bytes for them, and marks them.
    let stale = Stale::of(HashField::Value).with(HashField::KeyValue);
    let new: Vec<Vec<u8>> = (0..12u8).map(|index| vec![b'n', index]).collect();
    let tx = world.begin();
    let mut merk = world.merk(&tx, 1, |items| {
        for key in &new {
            items.insert(key.clone(), value(&mut rng, 80));
        }
    });
    merk.stale = new.iter().map(|key| (key.clone(), stale)).collect();
    let batch = OwnedBatch {
        records: World::root_record(&merk).into_iter().collect(),
        merks: vec![merk],
    };
    world.apply(&tx, &batch).expect("nodes with stale hashes");
    for key in &new {
        assert_eq!(shown(&tx, key), (stale, UNMADE), "{key:?}");
    }
    assert_eq!(shown(&tx, &[b'a', 3]).0, Stale::NONE);
    assert_eq!(tx.txn.stats().stale_nodes, new.len());

    // Later writes cut the pages, so cells are carried, and write some of
    // those nodes again with every hash made: those are stale no more.
    let mark = tx.txn.mark();
    let writes = tx.model.borrow().writes.len();
    let later = world.merk(&tx, 1, |items| {
        for index in 0..60u8 {
            items.insert(vec![b'z', index], value(&mut rng, 150));
        }
        for key in [&new[0], &new[5]] {
            items.insert(key.clone(), b"another value, with its hashes".to_vec());
        }
    });
    let made: BTreeSet<Vec<u8>> = later.nodes.iter().map(|node| node.0.clone()).collect();
    let batch = OwnedBatch {
        records: World::root_record(&later).into_iter().collect(),
        merks: vec![later],
    };
    world.apply(&tx, &batch).expect("more nodes");
    let still: Vec<&Vec<u8>> = new.iter().filter(|key| !made.contains(*key)).collect();
    assert!(!still.is_empty() && still.len() < new.len(), "{still:?}");
    for key in &new {
        let expected = match made.contains(key) {
            true => Stale::NONE,
            false => stale,
        };
        assert_eq!(shown(&tx, key).0, expected, "{key:?}");
    }
    assert_eq!(tx.txn.stats().stale_nodes, still.len());

    // A rollback to the mark makes the frames again, and the marks with
    // them, from the write set.
    assert!(tx.txn.rewind(mark));
    tx.model.borrow_mut().writes.truncate(writes);
    for key in &new {
        assert_eq!(shown(&tx, key), (stale, UNMADE), "{key:?}");
    }
    assert_eq!(tx.txn.stats().stale_nodes, new.len());
    // And so does a rebase.
    let rebuilds = tx.txn.stats().rebuilds;
    world.edit_outside(
        2,
        |items| drop(items.insert(b"k".to_vec(), b"v".to_vec())),
        &[&tx],
    );
    for key in &new {
        assert_eq!(shown(&tx, key), (stale, UNMADE), "{key:?}");
    }
    let stats = tx.txn.stats();
    assert_eq!(
        (stats.stale_nodes, stats.rebuilds),
        (new.len(), rebuilds + 1)
    );

    // The commit makes each stale hash once, with the rules it is given:
    // a value hash and a key-value hash for each of the nodes.
    assert_eq!(rule_calls(), 0, "nothing was made before the commit");
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    assert_eq!(rule_calls(), 2 * new.len() as u64);
    assert_eq!(
        db.counts.stale_nodes.load(Ordering::Relaxed),
        new.len() as u64
    );
    // Every reader finds the hashes the rules made, and none stale.
    world.check_committed();
    let reader = world.begin();
    for key in &new {
        let (stale, value_hash) = shown(&reader, key);
        assert!(stale.is_empty() && value_hash != UNMADE, "{key:?}");
    }

    // Served by key after an overlap, a record says which of its hashes
    // are stale as well.
    let tx = world.begin();
    let mut merk = world.merk(&tx, 1, |items| {
        drop(items.insert(b"late".to_vec(), value(&mut rng, 40)))
    });
    merk.stale.insert(b"late".to_vec(), stale);
    let batch = OwnedBatch {
        records: World::root_record(&merk).into_iter().collect(),
        merks: vec![merk],
    };
    world.apply(&tx, &batch).expect("a node with stale hashes");
    world.edit_outside(1, |items| drop(items.remove(&vec![b'a', 0])), &[&tx]);
    assert_eq!(shown(&tx, b"late"), (stale, UNMADE));
    assert!(tx.txn.stats().overlapped > 0 && tx.txn.stats().stale_nodes == 0);
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);
    assert_eq!(db.private_ids(), 0);
}
