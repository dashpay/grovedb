//! Where a transaction last saw its nodes (`crate::txn::places`,
//! `transactions.md` PN8): what makes an entry of the table of places,
//! what a write, a removal and a load do to one, what empties the table,
//! and that neither a place nor a key with no cell is taken across what
//! ends a lease. Every read gives what the read rule names.
//!
//! And what a caller tells of a node's place ([`WriteTxn::place`]): a
//! hint for the write that names the key and no handle. The cases that
//! go through `told_and_not` are run twice, with the telling and without
//! it, on stores that are otherwise the same: the two end with the same
//! records and the same counts of the transaction, and differ by the
//! searches the place spared.

use lignin_format::tree_page::Side;
use lignin_tree::node::{Lease, NodeHandle};

use super::{
    super::{frames::probe, pages::held},
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
fn a_written_node_is_kept_from_the_first_search_for_it() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(53);
    fill(&mut world, &mut rng, 1, 60);
    let tx = world.begin();
    let new = below_no_node(&world, &tx, 1);
    let put = |value: &[u8]| {
        let before = searches(&world);
        world.edit(&tx, 1, |items| {
            items.insert(new.clone(), value.to_vec());
        });
        searches(&world) - before
    };

    // A first write, of nodes that were not read before: the model reads
    // each node whose value it keeps by its key, and those keys are kept.
    // The new key is searched for by the write itself, where it is
    // applied, and nothing is kept of it: not that it had no cell, and
    // not where the write put its node.
    put(b"one");
    let kept = tx.txn.places();
    assert_eq!(kept.0, 0, "the frames took the places with the lease");
    assert_ne!(root(&world, &tx, 1), new);

    // A second write of the key, which nothing has asked for: the nodes
    // above it are where the first write left them, and the write
    // searches for the key once more.
    assert_eq!(put(b"two"), 1);
    assert_eq!(tx.txn.places(), kept);

    // The first read of it by its key searches, as for a node that the
    // transaction never wrote, and keeps what it finds. A write of the
    // node finds it there from then on, and keeps where it leaves it.
    assert_eq!(read_checked(&world, &tx, 1, &new), (1, false));
    assert_eq!(read_checked(&world, &tx, 1, &new), (0, true));
    assert_eq!(tx.txn.places(), (0, kept.1 + 1));
    assert_eq!(put(b"three"), 0);
    assert_eq!(read_checked(&world, &tx, 1, &new), (0, true));
    assert_eq!(tx.txn.places(), (0, kept.1 + 1));

    // The key is removed, and put again with no read in between. The
    // table has nothing of the key then, so the write searches for it,
    // and it has the key still, so it keeps where the write leaves the
    // node.
    world.edit(&tx, 1, |items| drop(items.remove(&new)));
    assert_eq!(put(b"four"), 1);
    assert_eq!(read_checked(&world, &tx, 1, &new), (0, true));

    // It is removed and asked for: it has no cell, which is searched for
    // once. A write that puts it then searches for nothing, and it is
    // read where the write left it.
    world.edit(&tx, 1, |items| drop(items.remove(&new)));
    assert_eq!(read_checked(&world, &tx, 1, &new), (1, false));
    assert_eq!(read_checked(&world, &tx, 1, &new), (0, false));
    assert_eq!(put(b"five"), 0);
    assert_eq!(read_checked(&world, &tx, 1, &new), (0, true));
    world.check(&tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
}

/// The nodes of the Merk `name` from its root to the node under `key`,
/// each with its handle, reached by the links of the nodes, as a writer
/// has them that walks the tree: the transaction searches for no key on
/// the way, and keeps no place of a node below the root for it.
fn walk(world: &World<'_>, tx: &Tx<'_>, name: u8, key: &[u8]) -> Vec<(Vec<u8>, NodeHandle)> {
    let top = root(world, tx, name);
    let handle = read(tx, name, &top).and_then(|(_, handle)| handle);
    let mut way = vec![(top, handle.expect("the root has a handle"))];
    loop {
        let at = way.last().expect("the root");
        if at.0 == key {
            return way;
        }
        let side = match key < &at.0[..] {
            true => Side::Left,
            false => Side::Right,
        };
        let seen = |view: Option<NodeView<'_>>| {
            let view = view.expect("a child on the way to the key");
            (view.key().to_vec(), view.handle().expect("a handle"))
        };
        let child = tx.txn.child(model::subtree(name), at.1, side, seen);
        way.push(child.expect("a read").expect("the handle is good"));
    }
}

/// A node that was searched for, and whose place has ended since, is
/// written by its key: a write carried a cell out of its page, as a split
/// does and as a merge does, and did not put the node. The write that
/// names it then searches for it, and keeps where it leaves it.
#[test]
fn a_write_of_a_node_whose_place_has_ended_keeps_the_new_place() {
    type Change = fn(&mut Items);
    let grow: Change = |items| {
        for index in 500..512 {
            items.insert(key(index), vec![7; 300]);
        }
    };
    let shrink: Change = |items| {
        for index in 20..70 {
            items.remove(&key(index));
        }
    };
    for (seed, change) in [(56, grow), (57, shrink)] {
        let (db, versions) = Db::new(96);
        let mut world = World::new(&db, &versions);
        let mut rng = Rng::new(seed);
        fill(&mut world, &mut rng, 1, 90);
        let tx = world.begin();
        let found: BTreeMap<Vec<u8>, NodeHandle> = (0..90)
            .map(|index| {
                let key = key(index);
                let (_, handle) = read(&tx, 1, &key).expect("a node");
                (key, handle.expect("a handle"))
            })
            .collect();
        let applied = world.edit(&tx, 1, change);
        assert!(!applied.carried.is_empty(), "{seed}: {applied:?}");

        // A node that the write left and did not put, whose handle of
        // before names its cell no more, below nodes whose places are
        // good: the write put them, or their handles are what they were.
        let left = items(&world, &tx, 1);
        let ended = |(key, now): &(Vec<u8>, NodeHandle)| {
            found.get(key) != Some(now) && !applied.nodes.contains(now)
        };
        let way = (left.keys())
            .map(|key| walk(&world, &tx, 1, key))
            .find(|way| {
                let (node, above) = way.split_last().expect("a node");
                ended(node) && !above.is_empty() && !above.iter().any(ended)
            })
            .expect("a node whose place has ended");
        let key = &way.last().expect("the node").0;
        assert!(found.contains_key(key), "{seed}: it was searched for");

        // The model reads the nodes above it by their keys, each where it
        // is kept, and the write names the node by its key alone.
        let (kept, before) = (tx.txn.places(), searches(&world));
        world.edit(&tx, 1, |items| {
            items.insert(key.clone(), b"written again".to_vec());
        });
        assert_eq!(searches(&world) - before, 1, "{seed}: {key:?}");
        assert_eq!(read_checked(&world, &tx, 1, key), (0, true), "{seed}");
        assert_eq!(tx.txn.places(), kept, "{seed}: no key is new to the table");
        world.check(&tx);
        assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
        world.check_committed();
    }
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
    assert_eq!(read_checked(&world, &tx, 1, &key(701)), (1, false));
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

/// The handle of the node under `key` of the Merk `name` as a writer has
/// it that walks the tree ([`walk`]).
fn walked(world: &World<'_>, tx: &Tx<'_>, name: u8, key: &[u8]) -> NodeHandle {
    walk(world, tx, name, key).last().expect("the node").1
}

/// A key of the Merk `name` that is not its root, and not `but`.
fn below_the_root(world: &World<'_>, tx: &Tx<'_>, name: u8, but: &[u8]) -> Vec<u8> {
    let top = root(world, tx, name);
    let keys = (0..60).map(key);
    let mut keys = keys.filter(|key| *key != top && key[..] != *but);
    keys.next().expect("a Merk of many nodes")
}

/// One store with the Merks 1 and 2, sixty nodes each, and a transaction
/// that has read every node of both by its key but `gone`, a node of the
/// Merk 1 below its root: with frames, where `framed` says so. `between`
/// is run then, and is given whether to tell what it has to tell. Then
/// the node under `gone` is removed, by its key and with no handle.
///
/// Returns the searches that the removal made and what the transaction
/// held after it. What the transaction reads is checked against the
/// model afterwards: a check before it would read `gone` by its key.
fn removal<F>(framed: bool, tell: bool, between: F) -> (u64, TxnStats)
where
    F: for<'s> FnOnce(&mut World<'s>, &Tx<'s>, &[u8], bool),
{
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(61);
    fill(&mut world, &mut rng, 1, 60);
    fill(&mut world, &mut rng, 2, 60);
    let tx = world.begin();
    if framed {
        world.write(&tx, vec![put(model::unit(Column::Aux, 9), b"k", b"v")]);
    }
    let gone = below_the_root(&world, &tx, 1, b"");
    for name in [1, 2] {
        for key in (0..60).map(key).filter(|key| (name, key) != (1, &gone)) {
            read(&tx, name, &key).expect("a node");
        }
    }
    between(&mut world, &tx, &gone, tell);
    let before = searches(&world);
    world.edit(&tx, 1, |items| drop(items.remove(&gone)));
    let made = searches(&world) - before;
    world.check(&tx);
    assert_eq!(read(&tx, 1, &gone), None);
    (made, tx.txn.stats())
}

/// Runs a case with the telling and without it. The two end with the same
/// counts of the transaction. Returns the searches of the removal in the
/// two.
fn told_and_not<F>(framed: bool, between: F) -> (u64, u64)
where
    F: for<'s> Fn(&mut World<'s>, &Tx<'s>, &[u8], bool),
{
    let (told, with) = removal(framed, true, &between);
    let (untold, without) = removal(framed, false, &between);
    assert_eq!(with, without, "the transaction holds the same either way");
    (told, untold)
}

fn merk(name: u8) -> Subtree {
    model::subtree(name)
}

#[test]
fn a_told_place_spares_the_search_for_a_node_that_is_removed() {
    for framed in [true, false] {
        // The node is where its writer saw it: the removal looks for no
        // key. With no frames yet, the first write takes what was told
        // with the lease it was told under.
        let right = told_and_not(framed, |world, tx, gone, tell| {
            let handle = walked(world, tx, 1, gone);
            let before = tx.txn.places();
            if tell {
                tx.txn.place(merk(1), gone, handle);
                let kept = tx.txn.places();
                assert_eq!(kept.0 + kept.1, before.0 + before.1 + 1);
                // The same place told again, and another for the key:
                // the first stands.
                let other = below_the_root(world, tx, 1, gone);
                tx.txn.place(merk(1), gone, handle);
                tx.txn.place(merk(1), gone, walked(world, tx, 1, &other));
                assert_eq!(tx.txn.places(), kept);
            } else {
                walked(world, tx, 1, &below_the_root(world, tx, 1, gone));
            }
        });
        assert_eq!(right, (0, 1), "{framed}");
    }
}

/// A told key is a key of the table. A write that puts the node under it
/// follows the told place, and keeps where it leaves the node: a read by
/// key is answered there from then on. With nothing told the write
/// searches for the key and keeps nothing, and the first read by key
/// searches once more.
#[test]
fn a_write_keeps_the_place_of_a_node_under_a_told_key() {
    for tell in [true, false] {
        let (db, versions) = Db::new(256);
        let mut world = World::new(&db, &versions);
        let mut rng = Rng::new(62);
        fill(&mut world, &mut rng, 1, 60);
        let tx = world.begin();
        world.write(&tx, vec![put(model::unit(Column::Aux, 9), b"k", b"v")]);
        let there = below_the_root(&world, &tx, 1, b"");
        for key in (0..60).map(key).filter(|key| *key != there) {
            read(&tx, 1, &key).expect("a node");
        }
        let handle = walked(&world, &tx, 1, &there);
        if tell {
            tx.txn.place(merk(1), &there, handle);
        }
        let kept = tx.txn.places();
        let before = searches(&world);
        world.edit(&tx, 1, |items| {
            items.insert(there.clone(), b"put again".to_vec());
        });
        assert_eq!(searches(&world) - before, u64::from(!tell), "{tell}");
        assert_eq!(tx.txn.places(), kept, "{tell}");
        let first = read_checked(&world, &tx, 1, &there);
        assert_eq!(first, (u64::from(!tell), tell), "{tell}");
        assert_eq!(read_checked(&world, &tx, 1, &there), (0, true), "{tell}");
        world.check(&tx);
        assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
        world.check_committed();
    }
}

#[test]
fn a_told_place_that_is_wrong_ends_as_the_search_ends() {
    // The cell of another node of the Merk: its key is another.
    let wrong = told_and_not(true, |world, tx, gone, tell| {
        let other = below_the_root(world, tx, 1, gone);
        let handle = walked(world, tx, 1, &other);
        if tell {
            tx.txn.place(merk(1), gone, handle);
        }
    });
    assert_eq!(wrong, (1, 1));

    // A cell of another Merk, under another key.
    let elsewhere = told_and_not(true, |world, tx, gone, tell| {
        let other = below_the_root(world, tx, 2, gone);
        let handle = walked(world, tx, 2, &other);
        if tell {
            tx.txn.place(merk(1), gone, handle);
        }
    });
    assert_eq!(elsewhere, (1, 1));

    // A wrong place first and the right one after it: the first stands.
    let first = told_and_not(true, |world, tx, gone, tell| {
        let other = below_the_root(world, tx, 1, gone);
        let (wrong, right) = (walked(world, tx, 1, &other), walked(world, tx, 1, gone));
        if tell {
            tx.txn.place(merk(1), gone, wrong);
            tx.txn.place(merk(1), gone, right);
        }
    });
    assert_eq!(first, (1, 1));

    // A place that is told for a key the transaction keeps a place of
    // changes nothing: the node of that key is read where it was found.
    let kept = told_and_not(true, |world, tx, gone, tell| {
        let other = below_the_root(world, tx, 1, gone);
        let handle = walked(world, tx, 1, gone);
        if tell {
            tx.txn.place(merk(1), &other, handle);
        }
        assert_eq!(read_checked(world, tx, 1, &other), (0, true));
    });
    assert_eq!(kept, (1, 1));
}

#[test]
fn a_told_place_ends_with_its_cell_and_with_its_lease() {
    // A cell leaves the page of the told cell before the removal: another
    // node of the page is removed. The place is good no more.
    let left = told_and_not(true, |world, tx, gone, tell| {
        let handle = walked(world, tx, 1, gone);
        let top = root(world, tx, 1);
        let beside = (0..60).rev().map(key).find(|key| {
            *key != top && key[..] != *gone && walked(world, tx, 1, key).page() == handle.page()
        });
        let beside = beside.expect("another node in the page");
        if tell {
            tx.txn.place(merk(1), gone, handle);
        }
        world.edit(tx, 1, |items| drop(items.remove(&beside)));
        assert_ne!(
            walked(world, tx, 1, gone),
            handle,
            "the told handle has ended"
        );
    });
    assert!(left.0 == left.1 && left.1 > 0, "{left:?}");

    // A commit beside the transaction: its frames are made again, under
    // another lease, and nothing that was told is taken over.
    let rebased = told_and_not(true, |world, tx, gone, tell| {
        let handle = walked(world, tx, 1, gone);
        if tell {
            tx.txn.place(merk(1), gone, handle);
        }
        world.edit_outside(
            2,
            |items| drop(items.insert(key(900), b"beside".to_vec())),
            &[tx],
        );
        read(tx, 2, &key(900)).expect("the node of the commit");
        assert_eq!(tx.txn.stats().rebuilds, 1);
    });
    assert!(rebased.0 == rebased.1 && rebased.1 > 0, "{rebased:?}");

    // A handle of a lease that has ended is told: it is dropped.
    let late = told_and_not(true, |world, tx, gone, tell| {
        let handle = walked(world, tx, 1, gone);
        world.edit_outside(
            2,
            |items| drop(items.insert(key(900), b"beside".to_vec())),
            &[tx],
        );
        read(tx, 2, &key(900)).expect("the node of the commit");
        let before = tx.txn.places();
        if tell {
            tx.txn.place(merk(1), gone, handle);
        }
        assert_eq!(tx.txn.places(), before);
    });
    assert!(late.0 == late.1 && late.1 > 0, "{late:?}");

    // A write scope that is rolled back: the frames are made again.
    let rolled = told_and_not(true, |world, tx, gone, tell| {
        let handle = walked(world, tx, 1, gone);
        if tell {
            tx.txn.place(merk(1), gone, handle);
        }
        let scope = tx.txn.scope().expect("a scope");
        let batch = OwnedBatch {
            records: vec![put(model::unit(Column::Aux, 9), b"rolled", b"back")],
            ..OwnedBatch::default()
        };
        batch.with(|batch| scope.apply(batch)).expect("a write");
        scope.rollback();
        let _ = world;
    });
    assert!(rolled.0 == rolled.1 && rolled.1 > 0, "{rolled:?}");
}

#[test]
fn no_read_is_answered_from_a_told_place() {
    // The node is told and then read by its key: the read searches, as it
    // does with nothing told, and from then on the node is where the read
    // found it.
    let read_first = told_and_not(true, |world, tx, gone, tell| {
        let handle = walked(world, tx, 1, gone);
        if tell {
            tx.txn.place(merk(1), gone, handle);
        }
        assert_eq!(read_checked(world, tx, 1, gone), (1, false));
        assert_eq!(read_checked(world, tx, 1, gone), (0, true));
    });
    assert_eq!(read_first, (0, 0));

    // Before the first write too, where the committed pages are read.
    let committed = told_and_not(false, |world, tx, gone, tell| {
        let handle = walked(world, tx, 1, gone);
        if tell {
            tx.txn.place(merk(1), gone, handle);
        }
        assert_eq!(read_checked(world, tx, 1, gone), (1, false));
    });
    assert_eq!(committed, (0, 0));
}

#[test]
fn a_full_table_takes_no_told_place() {
    let full = told_and_not(true, |world, tx, gone, tell| {
        let handle = walked(world, tx, 1, gone);
        let mut absent = 0u32;
        while tx.txn.places().1 < crate::txn::places::KEPT_PLACES {
            absent += 1;
            assert!(absent as usize <= crate::txn::places::KEPT_PLACES);
            let key = [&b"z"[..], &absent.to_be_bytes()].concat();
            assert_eq!(read(tx, 2, &key), None);
        }
        let before = tx.txn.places();
        if tell {
            tx.txn.place(merk(1), gone, handle);
        }
        assert_eq!(tx.txn.places(), before);
        let _ = world;
    });
    assert_eq!(full, (1, 1));
}

/// A full table has room for a place that is told of a key it has
/// already with nothing kept of it. The node under a key that a read
/// searched for is removed, and the table is filled. A place that is
/// told then of a key that is new to the table is dropped, and the
/// handle of the removed node, told after it, goes to the entry of its
/// key. Neither changes what a read finds, since no read is answered
/// from a told place: the removed key is searched for, has no cell, and
/// is not searched for again, and the read of the key that was new
/// follows no place. What a write does with a told place is in the
/// cases of `told_and_not`.
#[test]
fn a_full_table_takes_a_place_told_of_a_key_it_has() {
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(63);
    fill(&mut world, &mut rng, 1, 60);
    fill(&mut world, &mut rng, 2, 60);
    let tx = world.begin();
    world.write(&tx, vec![put(model::unit(Column::Aux, 9), b"k", b"v")]);
    let gone = below_the_root(&world, &tx, 1, b"");
    let (_, handle) = read(&tx, 1, &gone).expect("a node");
    let handle = handle.expect("a handle");
    world.edit(&tx, 1, |items| drop(items.remove(&gone)));
    let unasked = below_the_root(&world, &tx, 2, b"");
    let other = walked(&world, &tx, 2, &unasked);

    // Every entry but that of the removed node keeps something.
    let mut absent = 0u32;
    while tx.txn.places().1 < crate::txn::places::KEPT_PLACES - 1 {
        absent += 1;
        assert!(absent as usize <= crate::txn::places::KEPT_PLACES);
        let key = [&b"z"[..], &absent.to_be_bytes()].concat();
        assert_eq!(read(&tx, 2, &key), None);
    }
    let full = tx.txn.places();
    tx.txn.place(merk(2), &unasked, other);
    assert_eq!(tx.txn.places(), full, "a key that is new to it");
    tx.txn.place(merk(1), &gone, handle);
    assert_eq!(tx.txn.places(), (0, full.1 + 1), "a key it has");

    assert_eq!(read_checked(&world, &tx, 1, &gone), (1, false));
    assert_eq!(read_checked(&world, &tx, 1, &gone), (0, false));
    assert!(!read_checked(&world, &tx, 2, &unasked).1);
    world.check(&tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
}

/// A table takes a told place of a key that is new to it while its
/// buffer has room for the key, and drops it where the buffer has none.
/// The keys here are long, and none of them has a node.
#[test]
fn a_table_takes_no_told_place_whose_key_its_buffer_has_no_room_for() {
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(64);
    fill(&mut world, &mut rng, 1, 60);
    fill(&mut world, &mut rng, 2, 60);
    let tx = world.begin();
    world.write(&tx, vec![put(model::unit(Column::Aux, 9), b"k", b"v")]);
    let there = below_the_root(&world, &tx, 1, b"");
    let handle = walked(&world, &tx, 1, &there);
    let long = |first: u8, len: usize| {
        let mut key = vec![b'z'; len];
        key[1] = first;
        key
    };
    let mebibyte = 1 << 20;
    assert_eq!(crate::txn::places::KEPT_KEY_BYTES, 16 * mebibyte);

    let before = tx.txn.places();
    tx.txn.place(merk(2), &long(0, mebibyte), handle);
    assert_eq!(tx.txn.places(), (0, before.1 + 1), "there is room for it");
    for first in 1..15 {
        assert_eq!(read(&tx, 2, &long(first, mebibyte)), None);
    }
    let kept = tx.txn.places();
    assert_eq!(kept, (0, before.1 + 15));
    tx.txn.place(merk(2), &long(99, 2 * mebibyte), handle);
    assert_eq!(
        tx.txn.places(),
        kept,
        "the key is dropped, and nothing else"
    );

    // The search for a key that the buffer has no room for empties the
    // table first.
    assert_eq!(read(&tx, 2, &long(98, 2 * mebibyte)), None);
    assert_eq!(tx.txn.places(), (0, 1));
    world.check(&tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
}

fn refusal(result: Result<Applied, TxnError>) -> BatchError {
    match result {
        Err(TxnError::Batch(error)) => error,
        other => panic!("a refused batch is expected, and not {other:?}"),
    }
}

/// The record of the node under `key` of the Merk `name` as `tx` reads it.
fn record(world: &World<'_>, tx: &Tx<'_>, name: u8, key: &[u8]) -> model::NodeRec {
    let unit = Unit::data(model::subtree(name));
    let view = tx.model.borrow().view(&world.model.records, unit);
    match view.get(key) {
        Some(model::Val::Node(node)) => node.clone(),
        other => panic!("a node is expected under {key:?}, and not {other:?}"),
    }
}

/// A store whose Merk 1 has sixty nodes, with a transaction that has its
/// frames, the root of the Merk, the node the root links on its left and
/// a node that this one links: a branch of the tree, and a node of it.
/// The two are read by their keys, which searches for them, and kept.
/// `test` is given the root's key, the branch's and that of the node
/// below it.
fn with_a_branch(seed: u64, test: impl for<'s> FnOnce(&World<'s>, &Tx<'s>, [Vec<u8>; 3])) {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(seed);
    fill(&mut world, &mut rng, 1, 60);
    let tx = world.begin();
    world.write(&tx, vec![put(model::unit(Column::Aux, 9), b"k", b"v")]);
    let least = items(&world, &tx, 1).keys().next().cloned();
    let way = walk(&world, &tx, 1, &least.expect("a node"));
    assert!(way.len() >= 3, "the least key is {} nodes down", way.len());
    let [top, branch, below] = [0, 1, 2].map(|at| way[at].0.clone());
    for key in [&branch, &below] {
        assert_eq!(read_checked(&world, &tx, 1, key), (1, false));
        assert_eq!(read_checked(&world, &tx, 1, key), (0, true));
    }
    test(&world, &tx, [top, branch, below]);
    tx.txn.rollback().expect("no scope is open");
    tx.forget();
}

/// A change set of the Merk 1 of `tx` that changes nothing yet.
fn change_set<'s>(world: &World<'s>, tx: &Tx<'s>) -> OwnedMerk {
    world.merk(tx, 1, |_| {})
}

fn batch_of(merk: OwnedMerk) -> OwnedBatch {
    OwnedBatch {
        merks: vec![merk],
        ..OwnedBatch::default()
    }
}

/// A change set may write a node without one of its links and neither
/// write nor remove the child: the branch below that link stays in its
/// pages, out of the tree. Such a write ends the handles and the places
/// of the lease before it, as one that drops a Merk does: the keys of the
/// branch are searched for, and have no node. A change set that links
/// the branch again is refused, since no search from the root finds the
/// child, and every read stays what the search gives.
#[test]
fn a_write_that_cuts_a_branch_off_ends_the_places_of_its_lease() {
    with_a_branch(65, |world, tx, [top, branch, below]| {
        let lease = walked(world, tx, 1, &top).lease();
        let whole = record(world, tx, 1, &top);
        assert!(whole.left.is_some(), "the root links the branch");
        let cut = model::NodeRec {
            left: None,
            ..whole.clone()
        };
        let mut merk = change_set(world, tx);
        merk.nodes.push((top.clone(), cut, true));
        probe::lost_counts();
        let applied = (world.apply(tx, &batch_of(merk))).expect("the root is written");
        let left_out = probe::Lost {
            left_out: 1,
            ..probe::Lost::default()
        };
        assert_eq!(probe::lost_counts(), left_out);
        // The handle that the write reports of the root is of the lease
        // that ended with it.
        assert_ne!(walked(world, tx, 1, &top).lease(), lease);
        assert_eq!(applied.nodes.len(), 1);
        assert_eq!(applied.nodes[0].lease(), lease);

        let placed = tx.txn.stats().places_followed;
        for key in [&branch, &below] {
            assert_eq!(read(tx, 1, key), None, "{key:?}");
            assert_eq!(read(tx, 1, key), None, "{key:?}");
        }
        assert_eq!(tx.txn.stats().places_followed, placed);

        // The change set that makes the Merk what its items say again:
        // the root as it was, with its link to the branch.
        let merk = change_set(world, tx);
        let written: Vec<_> = merk
            .nodes
            .iter()
            .map(|(key, node, _)| (key, node))
            .collect();
        assert_eq!(written, [(&top, &whole)]);
        let subtree = model::subtree(1);
        let again = world.apply(tx, &batch_of(merk));
        assert_eq!(refusal(again), BatchError::NoChild { subtree });
        for key in [&branch, &below] {
            assert_eq!(read(tx, 1, key), None, "{key:?}");
        }
        assert!(read(tx, 1, &top).is_some());
    });
}

/// The same of a node that is removed while the nodes it linked are
/// neither removed nor linked anew, and of a root that is set to none
/// over cells that stay: the cells that are left out of the tree are
/// shown to no read, also once the Merk has a root again.
#[test]
fn a_write_that_leaves_cells_below_a_removed_node_or_a_root_ends_the_places() {
    with_a_branch(66, |world, tx, [top, branch, below]| {
        let lease = walked(world, tx, 1, &top).lease();
        let cut = model::NodeRec {
            left: None,
            ..record(world, tx, 1, &top)
        };
        let mut merk = change_set(world, tx);
        merk.nodes.push((top.clone(), cut, true));
        merk.removed.push(branch.clone());
        (world.apply(tx, &batch_of(merk))).expect("the branch's node is removed");
        assert_ne!(walked(world, tx, 1, &top).lease(), lease);
        assert_eq!(read(tx, 1, &branch), None);
        assert_eq!(read(tx, 1, &below), None);
    });

    with_a_branch(67, |world, tx, [top, branch, below]| {
        let leaf = model::NodeRec {
            left: None,
            right: None,
            ..record(world, tx, 1, &top)
        };
        let mut merk = change_set(world, tx);
        merk.root = Some(None);
        merk.removed.push(b"no node".to_vec());
        (world.apply(tx, &batch_of(merk))).expect("the Merk has no root");
        for key in [&top, &branch, &below] {
            assert_eq!(read(tx, 1, key), None, "{key:?}");
        }

        // A node of its own is the root: the cells that were left are in
        // their pages still, and none of them is a node of the Merk.
        let new = b"a new root".to_vec();
        let mut merk = change_set(world, tx);
        merk.nodes.push((new.clone(), leaf, true));
        merk.root = Some(Some(new.clone()));
        (world.apply(tx, &batch_of(merk))).expect("the Merk has a root");
        assert!(read(tx, 1, &new).is_some());
        for key in [&top, &branch, &below] {
            assert_eq!(read(tx, 1, key), None, "{key:?}");
        }
    });
}

/// A commit of a Merk that is made of the tree it is written to leaves no
/// cell out of its tree: it puts or removes every node that leaves, and
/// links anew what a rotation moves. Its write takes no new lease,
/// whatever it removes and puts: a handle ends then with a cell that
/// leaves its page, and with nothing else. Every
/// cell that lost its link in these commits was taken out by its change
/// set, or was the root after it, or was linked again from the root by
/// the nodes of the change set.
#[test]
fn a_commit_of_a_merk_takes_no_new_lease() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(68);
    fill(&mut world, &mut rng, 1, 90);
    let tx = world.begin();
    world.write(&tx, vec![put(model::unit(Column::Aux, 9), b"k", b"v")]);
    let lease = |world: &World<'_>| {
        let top = root(world, &tx, 1);
        walked(world, &tx, 1, &top).lease()
    };
    let before = lease(&world);
    probe::lost_counts();
    for round in 0..6 {
        world.edit(&tx, 1, |items| {
            for index in (round * 15)..(round * 15 + 11) {
                items.remove(&key(index));
            }
            for index in 0..7 {
                items.insert(key(500 + round * 10 + index), vec![round as u8; 90]);
            }
        });
        assert_eq!(lease(&world), before, "{round}");
        world.check(&tx);
    }
    world.edit(&tx, 1, |items| items.clear());
    world.edit(&tx, 1, |items| {
        items.insert(key(1), b"one".to_vec());
    });
    assert_eq!(lease(&world), before);
    let lost = probe::lost_counts();
    assert!(lost.held >= 6, "{lost:?}");
    assert_eq!(lost.left_out, 0);
    world.check(&tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
}

/// What the table answers is held to the search from the root of the
/// Merk in this build (`pages::held`): the place that a read follows and
/// a key that has no cell, with no frames and with them, and a place
/// that was told and that a write follows. The search of the hold is
/// counted as no keyed lookup.
#[test]
fn every_answer_of_the_table_is_held_to_the_search_from_the_root() {
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(71);
    fill(&mut world, &mut rng, 1, 60);
    let tx = world.begin();
    let there = below_the_root(&world, &tx, 1, b"");
    let none = below_no_node(&world, &tx, 1);
    for framed in [false, true] {
        if framed {
            world.write(&tx, vec![put(model::unit(Column::Aux, 9), b"k", b"v")]);
        }
        read(&tx, 1, &there).expect("a node");
        assert_eq!(read(&tx, 1, &none), None);
        held::compared();
        let before = searches(&world);
        read(&tx, 1, &there).expect("a node");
        assert_eq!(read(&tx, 1, &none), None);
        let compared = held::compared();
        let expected = held::Compared {
            kept: 1,
            absent: 1,
            told: 0,
            evaluated: 0,
        };
        assert_eq!(compared, expected, "{framed}");
        assert_eq!(searches(&world), before, "{framed}");
    }
    let gone = below_the_root(&world, &tx, 1, &there);
    tx.txn.place(merk(1), &gone, walked(&world, &tx, 1, &gone));
    held::compared();
    world.edit(&tx, 1, |items| drop(items.remove(&gone)));
    assert_eq!(held::compared().told, 1);
    world.check(&tx);
}

/// A place that is told for a key of one Merk, and that is of the node
/// which another Merk has under the same key, passes every check of a
/// handle ([`WriteTxn::place`]): the write would take that cell for the
/// node's. The hold fails the write, with the cell that was told and the
/// cell that the key leads to.
#[test]
fn the_hold_fails_a_told_place_that_is_of_another_merk() {
    let failed = std::panic::catch_unwind(|| {
        removal(true, true, |world, tx, gone, _| {
            tx.txn.place(merk(1), gone, walked(world, tx, 2, gone));
        })
    });
    let failed = failed.expect_err("the hold fails the removal");
    let message = failed.downcast_ref::<String>().expect("a message");
    assert!(message.contains("answers Told("), "{message}");
    assert!(message.contains("finds Some("), "{message}");
}

/// A write that cuts a branch off can put a node of that branch in the
/// same change set. Under a root that is kept the node has a cell, is
/// taken to stay in its tree, and is reported. Its handle ends with the
/// write, as every other does: nothing is shown by it, a place that is
/// told with it is dropped, and a change set that names it to link the
/// branch again finds no node under the key, and is refused for the child
/// that no search from the root finds.
#[test]
fn a_node_that_is_put_into_a_branch_its_write_cuts_off_is_reported_with_an_ended_handle() {
    with_a_branch(69, |world, tx, [top, branch, below]| {
        let lease = walked(world, tx, 1, &top).lease();
        let whole = record(world, tx, 1, &top);
        let node = record(world, tx, 1, &branch);
        assert_eq!(node.left.as_ref().map(|link| &link.key), Some(&below));
        let cut = model::NodeRec {
            left: None,
            ..whole.clone()
        };
        let mut set = change_set(world, tx);
        set.nodes.push((top.clone(), cut, true));
        set.nodes.push((branch.clone(), node.clone(), true));
        let applied = (world.apply(tx, &batch_of(set))).expect("both nodes are written");
        assert_eq!((applied.nodes.len(), applied.unlinked.len()), (2, 0));
        let now = walked(world, tx, 1, &top).lease();
        assert_ne!(now, lease);
        assert!(applied.nodes.iter().all(|handle| handle.lease() == lease));

        let reported = applied.nodes[1];
        let shown = |view: Option<NodeView<'_>>| view.is_some();
        let child = tx.txn.child(model::subtree(1), reported, Side::Left, shown);
        assert_eq!(child.expect("a read"), None);
        let places = tx.txn.places();
        tx.txn.place(merk(1), &branch, reported);
        assert_eq!(tx.txn.places(), places);
        for key in [&branch, &below] {
            assert_eq!(read(tx, 1, key), None, "{key:?}");
        }

        let mut again = change_set(world, tx);
        again.nodes = vec![(top.clone(), whole, true), (branch.clone(), node, true)];
        again.hints.insert(branch.clone(), reported);
        let subtree = model::subtree(1);
        let refused = world.apply_as_read(tx, &batch_of(again));
        assert_eq!(refusal(refused), BatchError::NoChild { subtree });
        for key in [&branch, &below] {
            assert_eq!(read(tx, 1, key), None, "{key:?}");
        }
        assert!(read(tx, 1, &top).is_some());
    });
}

/// A cell that loses its link is in its tree still where a node of the
/// tree links it, and not where any node of the batch does. The root
/// stops linking the branch, and a leaf of that branch is written with a
/// link to it: the leaf has a cell and its link names the branch, and
/// the two are in no tree. No link of the batch leads from the root to
/// the leaf, so its link holds nothing, and the handles end.
#[test]
fn a_link_from_a_node_that_no_tree_reaches_holds_no_cell() {
    with_a_branch(70, |world, tx, [top, branch, below]| {
        let least = items(world, tx, 1).keys().next().cloned().expect("a node");
        let leaf = record(world, tx, 1, &least);
        assert_eq!((&leaf.left, &leaf.right), (&None, &None));
        let lease = walked(world, tx, 1, &top).lease();
        let whole = record(world, tx, 1, &top);
        let cut = model::NodeRec {
            left: None,
            ..whole.clone()
        };
        let back = model::NodeRec {
            left: whole.left.clone(),
            ..leaf
        };
        let mut set = change_set(world, tx);
        set.nodes.push((top.clone(), cut, true));
        set.nodes.push((least.clone(), back, true));
        probe::lost_counts();
        let applied = (world.apply(tx, &batch_of(set))).expect("the two nodes are written");
        assert_eq!((applied.nodes.len(), applied.unlinked.len()), (2, 0));
        let left_out = probe::Lost {
            left_out: 1,
            ..probe::Lost::default()
        };
        assert_eq!(probe::lost_counts(), left_out);
        assert_ne!(walked(world, tx, 1, &top).lease(), lease);
        let placed = tx.txn.stats().places_followed;
        for key in [&branch, &below, &least] {
            assert_eq!(read(tx, 1, key), None, "{key:?}");
            assert_eq!(read(tx, 1, key), None, "{key:?}");
        }
        assert_eq!(tx.txn.stats().places_followed, placed);
    });
}

/// A change set that moves cells between nodes of its own, as a rotation
/// does, and that does not write the nodes above them: the nodes that
/// link the moved cells anew hang below cells the batch does not write,
/// and no link of the batch leads to them from the root. The cells are
/// in the tree all the same. Each lost the link that led to it, and
/// nothing that the batch states shows it to be in the tree: no more is
/// asked, and the write ends the handles, its own with them. Every read
/// after it is the model's, by a search that is kept.
#[test]
fn a_change_set_that_moves_cells_below_cells_it_does_not_write_ends_the_handles() {
    with_a_branch(71, |world, tx, [top, branch, below]| {
        let lease = walked(world, tx, 1, &top).lease();
        let (upper, middle) = (record(world, tx, 1, &branch), record(world, tx, 1, &below));
        let low = middle
            .left
            .clone()
            .expect("the way to the least key goes on");
        let lower = record(world, tx, 1, &low.key);
        // The node on the way below `below` goes up to where `below` was,
        // and `below` hangs on its right with what it had there.
        let link_to = |key: &[u8], from: &model::LinkRec| model::LinkRec {
            key: key.to_vec(),
            ..from.clone()
        };
        let up = upper.left.clone().expect("the branch links the node below");
        let nodes = vec![
            (
                branch.clone(),
                model::NodeRec {
                    left: Some(link_to(&low.key, &up)),
                    ..upper
                },
                true,
            ),
            (
                low.key.clone(),
                model::NodeRec {
                    right: Some(link_to(&below, &up)),
                    ..lower.clone()
                },
                true,
            ),
            (
                below.clone(),
                model::NodeRec {
                    left: lower.right.clone(),
                    ..middle
                },
                true,
            ),
        ];
        let mut set = change_set(world, tx);
        set.nodes = nodes;
        probe::lost_counts();
        let applied = (world.apply(tx, &batch_of(set))).expect("the three nodes are written");
        assert_eq!((applied.nodes.len(), applied.unlinked.len()), (3, 0));
        let left_out = probe::Lost {
            left_out: 1,
            ..probe::Lost::default()
        };
        assert_eq!(probe::lost_counts(), left_out);
        assert_ne!(walked(world, tx, 1, &top).lease(), lease);
        assert!(applied.nodes.iter().all(|handle| handle.lease() == lease));
        for key in [&branch, &below] {
            assert_eq!(read_checked(world, tx, 1, key), (1, false), "{key:?}");
            assert_eq!(read_checked(world, tx, 1, key), (0, true), "{key:?}");
        }
        for index in 0..60 {
            read_checked(world, tx, 1, &key(index));
        }
    });
}

/// A write that leaves a cell out of the tree of one Merk and loads
/// another Merk whole in the same batch: the handles of the load's nodes
/// are of the lease that ends with the write, as the others it reports
/// are, and the nodes of the load are read by their keys.
#[test]
fn a_write_that_cuts_a_branch_off_and_loads_a_merk_reports_ended_handles_for_both() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(72);
    fill(&mut world, &mut rng, 1, 60);
    fill(&mut world, &mut rng, 2, 4);
    let tx = world.begin();
    world.edit(&tx, 2, |items| items.clear());
    let top = root(&world, &tx, 1);
    let lease = walked(&world, &tx, 1, &top).lease();
    let whole_top = record(&world, &tx, 1, &top);
    assert!(whole_top.left.is_some(), "the root links a branch");
    let mut cut = world.merk(&tx, 1, |_| {});
    let without = model::NodeRec {
        left: None,
        ..whole_top
    };
    cut.nodes.push((top.clone(), without, true));
    let mut loaded = Items::new();
    for index in 0..12 {
        loaded.insert(key(index), value(&mut rng, 80));
    }
    let mut batch = super::load::batch_of(vec![whole(2, &loaded)]);
    batch.merks.insert(0, cut);
    let scope = tx.txn.scope().expect("a scope");
    let first_write = world.latest().get();
    let applied = batch.loading(&[model::subtree(2)], |batch| scope.apply(batch));
    scope.merge();
    let applied = applied.expect("a cut and a load");
    assert!(applied.load.is_some());
    world.note(&tx, &batch, first_write);
    assert_eq!(applied.nodes.len(), 1 + loaded.len());
    assert!(applied.nodes.iter().all(|handle| handle.lease() == lease));
    assert_ne!(walked(&world, &tx, 1, &top).lease(), lease);
    for key in loaded.keys() {
        read_checked(&world, &tx, 2, key);
    }
    tx.txn.rollback().expect("no scope is open");
    tx.forget();
}

/// The report of a write is of the lease the write was made under, and
/// that lease is the one that is dated at the write, also where it has
/// ended by then. One batch clears a Merk, so it is made under a lease
/// that no node was read under, and writes the root of another Merk
/// without one of its links, so that lease ends with the write. A later
/// write that keeps the value of the root, and names the handle the
/// first write reported as its read, names that write: it is no keep of
/// a value that a commit may have replaced.
#[test]
fn a_keep_by_a_handle_that_a_cutting_write_reported_is_dated_at_that_write() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(73);
    fill(&mut world, &mut rng, 1, 60);
    fill(&mut world, &mut rng, 2, 8);
    let tx = world.begin();
    let top = root(&world, &tx, 1);
    let whole = record(&world, &tx, 1, &top);
    assert!(whole.left.is_some(), "the root links a branch");
    let cut = model::NodeRec {
        left: None,
        ..whole
    };
    let mut set = world.merk(&tx, 1, |_| {});
    set.nodes.push((top.clone(), cut.clone(), false));
    let batch = OwnedBatch {
        merks: vec![set],
        subtrees: vec![OwnedSubtree::Clear(model::subtree(2))],
        records: vec![delete(Unit::new(Column::Roots, model::subtree(2)), b"root")],
    };
    let applied = world.apply(&tx, &batch).expect("a clear and a cut");
    assert_eq!(applied.nodes.len(), 1);
    let reported = applied.nodes[0];
    assert_ne!(walked(&world, &tx, 1, &top).lease(), reported.lease());

    let mut again = world.merk(&tx, 1, |_| {});
    again.nodes = vec![(top.clone(), cut, false)];
    again.hints.insert(top, reported);
    (world.apply_as_read(&tx, &batch_of(again))).expect("the keep names the write");
    assert_eq!(tx.txn.stats().overlapped, 0);
    tx.txn.rollback().expect("no scope is open");
    tx.forget();
}

/// The same of two writes whose trees stay in key order throughout. The
/// first stops linking the branch on the root's left and puts the node
/// of the branch: the node has a cell, is reported, and its handle ends.
/// The node on the root's right is searched for then, and kept. The
/// second write stops linking that node from the root and links it from
/// the node of the cut branch, which it names by the handle the first
/// write reported: the handle is not followed, no node is under the key,
/// and what is put there is linked by no node of the tree, so it is a
/// record beside the tree and its link holds nothing. The handles end
/// again, and the node that was on the right is no node of the tree.
#[test]
fn a_link_from_a_node_of_a_branch_that_was_cut_off_before_holds_no_cell() {
    with_a_branch(74, |world, tx, [top, branch, _]| {
        let whole = record(world, tx, 1, &top);
        let right = whole
            .right
            .clone()
            .expect("the root links a node on its right");
        let node = record(world, tx, 1, &branch);
        let alone = model::NodeRec {
            left: None,
            ..whole.clone()
        };
        let mut set = change_set(world, tx);
        set.nodes.push((top.clone(), alone.clone(), true));
        set.nodes.push((branch.clone(), node.clone(), true));
        let applied = (world.apply(tx, &batch_of(set))).expect("the first write");
        assert_eq!((applied.nodes.len(), applied.unlinked.len()), (2, 0));
        let reported = applied.nodes[1];
        assert_eq!(read_checked(world, tx, 1, &right.key), (1, false));
        assert_eq!(read_checked(world, tx, 1, &right.key), (0, true));
        let lease = walked(world, tx, 1, &top).lease();
        assert_ne!(reported.lease(), lease);

        let bare = model::NodeRec {
            right: None,
            ..alone
        };
        let holder = model::NodeRec {
            right: Some(right.clone()),
            ..node
        };
        let mut set = change_set(world, tx);
        set.nodes = vec![(top.clone(), bare, true), (branch.clone(), holder, true)];
        set.hints.insert(branch.clone(), reported);
        probe::lost_counts();
        let applied = (world.apply_as_read(tx, &batch_of(set))).expect("the second write");
        assert_eq!((applied.nodes.len(), &applied.unlinked[..]), (1, &[1][..]));
        let left_out = probe::Lost {
            left_out: 1,
            ..probe::Lost::default()
        };
        assert_eq!(probe::lost_counts(), left_out);
        assert_ne!(walked(world, tx, 1, &top).lease(), lease);
        let placed = tx.txn.stats().places_followed;
        assert_eq!(read(tx, 1, &right.key), None);
        assert_eq!(read(tx, 1, &right.key), None);
        assert_eq!(tx.txn.stats().places_followed, placed);
    });
}

/// A store whose Merk 1 has one node, under the one-byte key `sole`, and a
/// transaction that has its frames and has searched for that key and for
/// the key 255, which has no node. The transaction then writes a chain
/// over it, with its keys in order: the nodes 0 to `last`, each on the
/// right of the one before it, and the first as the root. Where `sole`
/// is past `last`, the node under it is put as well, as it is, and no
/// node of the chain links it. `test` is given the lease from before the
/// write, what the write reported and what it counted of the cell that
/// was the root.
fn with_a_chain(
    (sole, last): (usize, usize),
    test: impl for<'s> FnOnce(&World<'s>, &Tx<'s>, Lease, Applied, probe::Lost),
) {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let deep = |index: usize| vec![index as u8];
    world.edit_outside(
        1,
        |items| drop(items.insert(deep(sole), b"last".to_vec())),
        &[],
    );
    let tx = world.begin();
    world.write(&tx, vec![put(model::unit(Column::Aux, 9), b"k", b"v")]);
    let lease = read(&tx, 1, &deep(sole)).and_then(|(_, handle)| handle);
    let lease = lease.expect("the node has a handle").lease();
    assert_eq!(read(&tx, 1, &[255]), None);
    let leaf = record(&world, &tx, 1, &deep(sole));
    // A link as the model states one, to name each next node with.
    let three: Items = (0..3).map(|index| (deep(index), Vec::new())).collect();
    let stated = records(&three).1.into_values();
    let link = stated.flat_map(|node| node.left.or(node.right)).next();
    let link = link.expect("a tree of three nodes has a link");
    let mut set = world.merk(&tx, 1, |_| {});
    set.nodes = (0..=last)
        .map(|index| {
            let right = (index < last).then(|| model::LinkRec {
                key: deep(index + 1),
                ..link.clone()
            });
            (
                deep(index),
                model::NodeRec {
                    right,
                    ..leaf.clone()
                },
                true,
            )
        })
        .collect();
    if sole > last {
        set.nodes.push((deep(sole), leaf.clone(), true));
    }
    set.root = Some(Some(deep(0)));
    let batch = OwnedBatch {
        records: World::root_record(&set).into_iter().collect(),
        merks: vec![set],
        ..OwnedBatch::default()
    };
    probe::lost_counts();
    let applied = world.apply(&tx, &batch).expect("the chain is written");
    assert_eq!(applied.nodes.len(), last + 1);
    assert!(applied.nodes.iter().all(|handle| handle.lease() == lease));
    test(&world, &tx, lease, applied, probe::lost_counts());
    tx.txn.rollback().expect("no scope is open");
    tx.forget();
}

/// A change set can state a tree that is deeper than a search by key
/// goes, with its keys in order: a chain of 129 nodes over a Merk whose
/// one node becomes the last of them. The links of the batch lead to
/// that cell from the root, and further than a search comes: it is held
/// to be left out, and the handles end. What the transaction kept of the
/// key answers nothing then, and the read by key fails as the search
/// does, where the node one link nearer to the root is read.
#[test]
fn a_change_set_that_links_a_cell_deeper_than_a_search_goes_ends_the_handles() {
    with_a_chain((128, 128), |_, tx, lease, applied, lost| {
        assert!(applied.unlinked.is_empty());
        let left_out = probe::Lost {
            left_out: 1,
            ..probe::Lost::default()
        };
        assert_eq!(lost, left_out);
        let near = read(tx, 1, &[127]).and_then(|(_, handle)| handle);
        assert_ne!(near.expect("a node that a search comes to").lease(), lease);
        let last = tx
            .txn
            .node(model::subtree(1), &[128], |view| view.is_some());
        assert!(matches!(last, Err(TxnError::Damaged(_))), "{last:?}");
    });
}

/// The same with one node less: the cell that was the root is the last
/// that a search by key comes to, 127 links from the root. The links of
/// the batch hold it, and no lease is taken: its place answers, where
/// the search comes to it. A key past the end of the chain has no node,
/// by what was found before the write and by a search that ends at the
/// last cell, which has no child there.
#[test]
fn a_cell_that_a_change_set_links_as_deep_as_a_search_goes_stays_in_its_tree() {
    with_a_chain((127, 127), |world, tx, lease, applied, lost| {
        assert!(applied.unlinked.is_empty());
        let held = probe::Lost {
            held: 1,
            ..probe::Lost::default()
        };
        assert_eq!(lost, held);
        assert_eq!(read_checked(world, tx, 1, &[127]), (0, true));
        let last = read(tx, 1, &[127]).and_then(|(_, handle)| handle);
        assert_eq!(last.expect("the last node").lease(), lease);
        let (placed, searched) = (tx.txn.stats().places_followed, searches(world));
        assert_eq!(read(tx, 1, &[255]), None);
        assert_eq!(tx.txn.stats().places_followed, placed);
        assert_eq!(searches(world), searched, "what was found before answers");
        assert_eq!(read(tx, 1, &[254]), None);
        assert_eq!(searches(world), searched + 1);
    });
}

/// The walk that finds whether the tree reaches a written node ends
/// where the search does. The chain is as deep as a search goes, and the
/// node that was the root is put with it, past its end and linked by no
/// node: the walk for its key comes to the last cell of the chain, which
/// has no child there, and the tree does not reach the node. It is kept
/// as a record beside the tree, its cell goes, and no lease is taken.
#[test]
fn a_written_node_past_the_last_cell_that_a_search_comes_to_is_in_no_tree() {
    with_a_chain((200, 127), |_, tx, lease, applied, lost| {
        assert_eq!(applied.unlinked, [128]);
        let held = probe::Lost {
            held: 1,
            ..probe::Lost::default()
        };
        assert_eq!(lost, held);
        let last = read(tx, 1, &[127]).and_then(|(_, handle)| handle);
        assert_eq!(last.expect("the last node of the chain").lease(), lease);
        assert_eq!(read(tx, 1, &[200]), Some((b"last".to_vec(), None)));
    });
}
