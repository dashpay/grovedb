//! Where a transaction last saw its nodes (`crate::txn::places`,
//! `transactions.md` PN8): a node that was found by its key, or written, is
//! read and written again with no search from the root of its Merk, a key
//! that was found to have no cell is not searched for again, and neither is
//! taken across what ends a lease. Every read gives what the read rule
//! names.

use lignin_tree::node::NodeHandle;

use super::{
    load::whole,
    model::{self, records, value, OwnedSubtree},
    store::{Db, Outcome, Rng},
    *,
};

type Items = BTreeMap<Vec<u8>, Vec<u8>>;

fn key(index: usize) -> Vec<u8> {
    format!("n{index:04}").into_bytes()
}

fn fill(world: &mut World<'_>, rng: &mut Rng, name: u8, count: usize) {
    world.edit_outside(
        name,
        |items| {
            for index in 0..count {
                let len = 60 + rng.below(120);
                items.insert(key(index), value(rng, len));
            }
        },
        &[],
    );
}

/// The searches by key that went on below the root of a Merk, as the
/// store has counted them.
fn searches(world: &World<'_>) -> u64 {
    world.db.stats.keyed_lookups()
}

/// The items of the Merk `name` as `tx` reads them.
fn items(world: &World<'_>, tx: &Tx<'_>, name: u8) -> Items {
    let unit = Unit::data(model::subtree(name));
    let view = tx.model.borrow().view(&world.model.records, unit);
    Committed::items(&view)
}

/// The key of the root of the Merk `name` as `tx` reads it.
fn root(world: &World<'_>, tx: &Tx<'_>, name: u8) -> Vec<u8> {
    records(&items(world, tx, name))
        .0
        .expect("the Merk has a node")
}

/// A key that the Merk `name` has no node under, on a side of its root
/// on which the root has a child, and which would not be the root if a
/// node were put under it: a search for it goes on below the root, with
/// a node under it and with none.
fn below_no_node(world: &World<'_>, tx: &Tx<'_>, name: u8) -> Vec<u8> {
    let items = items(world, tx, name);
    let top = root(world, tx, name);
    let first = match items.keys().next_back() == Some(&top) {
        true => b'a',
        false => b'z',
    };
    (0..=u8::MAX)
        .map(|second| vec![first, second])
        .find(|key| {
            let mut with = items.clone();
            with.insert(key.clone(), Vec::new());
            records(&with).0.as_ref() == Some(&top)
        })
        .expect("a key that would not be the root")
}

/// Reads the node under `key` of the Merk `name` by its key, with no
/// handle, and returns its value and its handle.
fn read(tx: &Tx<'_>, name: u8, key: &[u8]) -> Option<(Vec<u8>, Option<NodeHandle>)> {
    let shown = |view: Option<NodeView<'_>>| {
        view.map(|view| {
            let mut buf = lignin_tree::value::ValueBuf::new();
            let value = view.value(&mut buf).expect("a value").to_vec();
            (value, view.handle())
        })
    };
    (tx.txn.node(model::subtree(name), key, shown)).expect("a read")
}

/// Reads the node under `key` by its key and checks it against the
/// model. Returns how many searches the read made, and whether it was
/// served by a place.
fn read_checked(world: &World<'_>, tx: &Tx<'_>, name: u8, key: &[u8]) -> (u64, bool) {
    let expected = items(world, tx, name).get(key).cloned();
    let (before, placed) = (searches(world), tx.txn.stats().places_followed);
    let found = read(tx, name, key).map(|(value, _)| value);
    assert_eq!(found, expected, "the node under {key:?}");
    let placed = tx.txn.stats().places_followed - placed;
    (searches(world) - before, placed == 1)
}

#[test]
fn a_node_is_searched_for_once_under_a_lease() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(51);
    fill(&mut world, &mut rng, 1, 90);
    let tx = world.begin();
    let top = root(&world, &tx, 1);
    let below: Vec<Vec<u8>> = (0..90).map(key).filter(|key| *key != top).collect();

    // The root is where the subtree directory says: no key chooses a
    // link, and no search is counted, the first time or later.
    assert_eq!(read_checked(&world, &tx, 1, &top), (0, false));
    assert_eq!(read_checked(&world, &tx, 1, &top), (0, true));

    // Any other node is searched for once, and read where it was found
    // from then on.
    for key in &below {
        assert_eq!(read_checked(&world, &tx, 1, key), (1, false), "{key:?}");
    }
    for key in &below {
        assert_eq!(read_checked(&world, &tx, 1, key), (0, true), "{key:?}");
    }
    assert_eq!(tx.txn.places(), (90, 0), "it has no frames yet");

    // So is a key that has no cell. The same key in another Merk is
    // another key.
    let absent = below_no_node(&world, &tx, 1);
    assert_eq!(read_checked(&world, &tx, 1, &absent), (1, false));
    assert_eq!(read_checked(&world, &tx, 1, &absent), (0, false));
    assert_eq!(read_checked(&world, &tx, 2, &absent), (0, false));
    assert_eq!(read_checked(&world, &tx, 2, &below[0]), (0, false));
    assert_eq!(tx.txn.stats().handles_followed, 0);
    assert_eq!(world.commit(tx, &[]), Outcome::Empty);
}

#[test]
fn the_first_write_follows_what_was_read_before_it() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(52);
    fill(&mut world, &mut rng, 1, 90);
    let tx = world.begin();
    let top = root(&world, &tx, 1);
    for index in 0..90 {
        read_checked(&world, &tx, 1, &key(index));
    }
    let new = below_no_node(&world, &tx, 1);
    assert_eq!(read_checked(&world, &tx, 1, &new), (1, false));

    // The write puts a node under the key that has none, and the nodes
    // above it with their values kept, each by the handle of its read.
    // Its frames are made under the lease of those reads: every node it
    // names is where its handle says, every child it links and does not
    // write is where the cell of its parent says, and the new key is known
    // to have no cell.
    let before = searches(&world);
    let applied = world.edit(&tx, 1, |items| {
        items.insert(new.clone(), b"a new node".to_vec());
    });
    assert!(applied.nodes.len() > 1, "{applied:?}");
    assert_eq!(searches(&world), before, "the write searched for nothing");
    assert_eq!(tx.txn.stats().rebuilds, 0);
    let (beside, framed) = tx.txn.places();
    assert_eq!(beside, 0, "the frames took the places with the lease");
    assert_eq!(framed, 91);

    // What it wrote is read where it left it, and what it did not write
    // where it was found before: but for the nodes in the pages that the
    // write carried a cell out of. A place into such a page has ended
    // with the cell's leaving (PN8), and a node there that the write did
    // not put is found by its key once more.
    assert_eq!(read_checked(&world, &tx, 1, &new), (0, true));
    let mut found_again = 0;
    for index in 0..90 {
        let key = key(index);
        match read_checked(&world, &tx, 1, &key) {
            (0, true) => {}
            (0 | 1, false) => found_again += 1,
            served => panic!("{key:?}: {served:?}"),
        }
    }
    assert!(!applied.carried.is_empty(), "{applied:?}");
    assert!((1..90).contains(&found_again), "{found_again}");
    for index in 0..90 {
        let key = key(index);
        assert_eq!(read_checked(&world, &tx, 1, &key), (0, true), "{key:?}");
    }
    assert_ne!(root(&world, &tx, 1), new, "{top:?} or another");
    world.check(&tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
}

#[test]
fn a_written_node_is_found_where_its_write_left_it() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(53);
    fill(&mut world, &mut rng, 1, 60);
    let tx = world.begin();

    // A first write, of nodes that were not read before: the model reads
    // each node whose value it keeps by its key, and the new key is
    // searched for where the write is applied.
    world.edit(&tx, 1, |items| {
        items.insert(key(1000), b"one".to_vec());
    });
    // A second write of the same Merk writes the same path again: every
    // node of it is where the first write left it.
    let before = searches(&world);
    assert_eq!(read_checked(&world, &tx, 1, &key(1000)), (0, true));
    world.edit(&tx, 1, |items| {
        items.insert(key(1000), b"two".to_vec());
    });
    assert_eq!(searches(&world), before);

    // A key that is removed has no place afterwards, and is searched for
    // once more: it has no cell then.
    world.edit(&tx, 1, |items| drop(items.remove(&key(1000))));
    assert_eq!(read_checked(&world, &tx, 1, &key(1000)), (1, false));
    assert_eq!(read_checked(&world, &tx, 1, &key(1000)), (0, false));

    // And it is put again where a later write puts it.
    world.edit(&tx, 1, |items| {
        items.insert(key(1000), b"three".to_vec());
    });
    assert_eq!(read_checked(&world, &tx, 1, &key(1000)), (0, true));
    world.check(&tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
}

#[test]
fn nothing_is_taken_across_what_ends_a_lease() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(54);
    fill(&mut world, &mut rng, 1, 60);
    fill(&mut world, &mut rng, 2, 10);
    let tx = world.begin();
    let (there, absent) = (key(7), below_no_node(&world, &tx, 1));
    assert_ne!(root(&world, &tx, 1), there);
    // An operation that reads no node below a root: where the frames are
    // made again, they are made here, and the write set is laid out
    // again by its keys.
    let wake = |world: &World<'_>, tx: &Tx<'_>| {
        let top = root(world, tx, 2);
        read_checked(world, tx, 2, &top);
    };
    let seen = |world: &World<'_>, tx: &Tx<'_>| {
        let there = read_checked(world, tx, 1, &there);
        let absent = read_checked(world, tx, 1, &absent);
        (there.0, absent.0)
    };
    assert_eq!(seen(&world, &tx), (1, 1));
    assert_eq!(seen(&world, &tx), (0, 0));

    // A commit beside a transaction that has written nothing: it reads
    // another version, under another lease. The key that had no cell has
    // one now.
    world.edit_outside(
        1,
        |items| drop(items.insert(absent.clone(), b"outside".to_vec())),
        &[&tx],
    );
    assert_eq!(seen(&world, &tx), (1, 1));
    assert_eq!(seen(&world, &tx), (0, 0));
    world.edit_outside(1, |items| drop(items.remove(&absent)), &[&tx]);
    assert_eq!(seen(&world, &tx), (1, 1));

    // A commit beside one that has frames: they are made again.
    world.edit(&tx, 2, |items| {
        items.insert(key(900), b"own".to_vec());
    });
    assert_eq!(seen(&world, &tx), (0, 0), "the frames took the places");
    world.edit_outside(
        1,
        |items| drop(items.insert(absent.clone(), b"again".to_vec())),
        &[&tx],
    );
    assert_eq!(tx.txn.stats().rebuilds, 0, "not before its next operation");
    wake(&world, &tx);
    assert_eq!(tx.txn.stats().rebuilds, 1);
    assert_eq!(seen(&world, &tx), (1, 1));
    assert_eq!(seen(&world, &tx), (0, 0));

    // A rollback to a mark, of a write that put a node under a key and
    // took the node of another away.
    let mark = tx.txn.mark();
    let writes = tx.model.borrow().writes.len();
    world.edit(&tx, 1, |items| {
        items.remove(&absent);
        items.remove(&there);
        items.insert(key(701), b"to be rolled back".to_vec());
    });
    assert_eq!(read_checked(&world, &tx, 1, &key(701)), (0, true));
    assert_eq!(seen(&world, &tx), (1, 1));
    assert_eq!(seen(&world, &tx), (0, 0));
    tx.txn.rewind(mark);
    tx.model.borrow_mut().writes.truncate(writes);
    wake(&world, &tx);
    assert_eq!(tx.txn.stats().rebuilds, 2);
    assert_eq!(seen(&world, &tx), (1, 1));
    assert_eq!(read_checked(&world, &tx, 1, &key(701)).0, 1);

    // A rollback of the whole transaction.
    world.edit(&tx, 1, |items| drop(items.remove(&there)));
    assert_eq!(read_checked(&world, &tx, 1, &there), (1, false));
    tx.txn.rollback().expect("no scope is open");
    tx.forget();
    assert_eq!(seen(&world, &tx), (1, 1));
    assert_eq!(tx.txn.places().1, 0, "it has no frames");
    world.check(&tx);
}

#[test]
fn a_merk_that_is_emptied_cleared_or_loaded_keeps_nothing_of_its_keys() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(55);
    fill(&mut world, &mut rng, 1, 40);
    fill(&mut world, &mut rng, 2, 40);
    let tx = world.begin();
    world.write(&tx, vec![put(model::unit(Column::Aux, 9), b"k", b"v")]);
    let (there, absent) = (key(7), key(700));
    for name in [1, 2] {
        assert_ne!(root(&world, &tx, name), there);
        assert_eq!(read_checked(&world, &tx, name, &there).0, 1);
        assert_eq!(read_checked(&world, &tx, name, &absent).0, 1);
        assert_eq!(read_checked(&world, &tx, name, &absent).0, 0);
    }
    let mut loaded = Items::new();
    for index in (0..40).chain([700]) {
        loaded.insert(key(index), value(&mut rng, 80));
    }
    let load = |name: u8| {
        let batch = super::load::batch_of(vec![whole(name, &loaded)]);
        let scope = tx.txn.scope().expect("a scope");
        let first_write = world.latest().get();
        let applied = batch.loading(&[model::subtree(name)], |batch| scope.apply(batch));
        scope.merge();
        assert!(applied.expect("a load").load.is_some());
        world.note(&tx, &batch, first_write);
    };

    // Every node of the first Merk is removed, under the lease the keys
    // were read under, and the Merk is loaded whole: with a node under
    // the key that was found to have none.
    world.edit(&tx, 1, |items| items.clear());
    assert_eq!(read_checked(&world, &tx, 1, &there), (0, false));
    load(1);
    let top = root(&world, &tx, 1);
    for index in (0..40).chain([700]) {
        let key = key(index);
        let served = read_checked(&world, &tx, 1, &key);
        assert_eq!(served, (u64::from(key != top), false), "{key:?}");
        assert_eq!(read_checked(&world, &tx, 1, &key), (0, true), "{key:?}");
    }

    // The second Merk is cleared: the node that was found is gone, though
    // its cell is where it was. Then it is loaded too.
    let cleared = OwnedBatch {
        subtrees: vec![OwnedSubtree::Clear(model::subtree(2))],
        records: vec![delete(Unit::new(Column::Roots, model::subtree(2)), b"root")],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &cleared).expect("a clear");
    assert_eq!(read_checked(&world, &tx, 2, &there), (0, false));
    assert_eq!(read_checked(&world, &tx, 2, &absent), (0, false));
    load(2);
    for found in [&there, &absent] {
        let served = read_checked(&world, &tx, 2, found);
        assert!(served.0 <= 1 && !served.1, "{served:?}");
        assert_eq!(read_checked(&world, &tx, 2, found), (0, true));
    }
    world.check(&tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
}
