//! A cursor that is kept apart from its scope
//! ([`Scope::parked_merk_cursor`], [`Scope::resume`]), and the keys of a
//! node's children as a cursor reads them with the node
//! ([`MerkCursor::node_with_children`]).

use std::sync::Mutex;

use super::{
    model::{self, value, Committed, OwnedBatch, OwnedMerk, OwnedSubtree},
    store::{Db, Outcome, Rng},
    *,
};

fn key(index: usize) -> Vec<u8> {
    format!("{index:04}").into_bytes()
}

/// A change of the Merk `name`, with the record its root key is written
/// to.
fn change<'s>(
    world: &World<'s>,
    tx: &Tx<'s>,
    name: u8,
    edit: impl FnOnce(&mut BTreeMap<Vec<u8>, Vec<u8>>),
) -> OwnedBatch {
    let merks = vec![world.merk(tx, name, edit)];
    OwnedBatch {
        records: merks.iter().filter_map(World::root_record).collect(),
        merks,
        ..OwnedBatch::default()
    }
}

/// A batch that clears the Merk `subtree` and writes it anew with `items`.
fn anew(subtree: Subtree, items: &BTreeMap<Vec<u8>, Vec<u8>>) -> OwnedBatch {
    let merk = OwnedMerk::change(subtree, &BTreeMap::new(), items);
    OwnedBatch {
        records: World::root_record(&merk).into_iter().collect(),
        merks: vec![merk],
        subtrees: vec![OwnedSubtree::Clear(subtree)],
    }
}

/// Applies `batch` through `scope`, which stays open, and to the model of
/// its transaction.
fn apply_through<'s>(world: &World<'s>, tx: &Tx<'s>, scope: &Scope<'_, 's>, batch: &OwnedBatch) {
    let first_write = world.latest().get();
    // Each node that keeps its value is read through the scope, as its
    // writer reads it, and the write names that read.
    let batch = &batch.through(scope);
    (batch.with(|batch| scope.apply(batch))).expect("the batch is applied");
    let writes = batch.writes().into_iter();
    (tx.model.borrow_mut().writes).extend(writes.map(|write| (write, first_write, false)));
}

/// RD6 for a parked cursor: it reads the Merk of the version it was opened
/// on, whatever is committed afterwards, with the writes of its scope as
/// they are at each call. Parked, it holds the pin of that version and no
/// page.
#[test]
fn a_parked_cursor_reads_its_version_and_holds_no_page() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(11);
    let subtree = model::subtree(1);
    let unit = Unit::data(subtree);
    world.edit_outside(
        1,
        |items| {
            for index in 0..40 {
                items.insert(key(index * 2), value(&mut rng, 60));
            }
        },
        &[],
    );
    let at_open = world.model.records.clone();
    let opened = world.latest();

    // The scope is behind a lock, as the handles of a writer keep it, and
    // the cursor is kept beside it.
    let tx = world.begin();
    let scope = Mutex::new(tx.txn.scope().expect("a scope"));
    let mut parked = (scope.lock().unwrap())
        .parked_merk_cursor(subtree)
        .expect("a cursor");
    assert!(format!("{parked:?}").starts_with("ParkedCursor"));
    {
        let scope = scope.lock().unwrap();
        let mut cursor = scope.resume(&mut parked).expect("its own scope");
        assert_eq!(cursor.version(), opened);
        assert!(!cursor.valid());
        cursor.seek_for_prev(&key(13)).expect("a seek");
        assert_eq!(cursor.key(), Some(&key(12)[..]));
        cursor.prev().expect("a step");
        assert!(cursor.valid());
        assert_eq!(cursor.key(), Some(&key(10)[..]));
    }

    // Commits outside, after it was opened: a new node, a delete, and the
    // whole Merk made again, which frees the pages it read.
    world.edit_outside(
        1,
        |items| {
            items.insert(key(11), b"outside".to_vec());
            items.remove(&key(12));
        },
        &[&tx],
    );
    for round in 0..3u8 {
        world.edit_outside(
            1,
            |items| {
                items.clear();
                for index in 0..70 {
                    items.insert(key(index), vec![round; 90]);
                }
            },
            &[&tx],
        );
    }
    assert!(world.latest() > opened);
    assert_eq!(
        db.oldest_pin(&versions),
        opened,
        "it keeps its version pinned"
    );
    assert_eq!(db.pool.census().pinned, 0, "and no page");

    // It is moved on another thread, through the scope, and goes on from
    // its key over the Merk of its version.
    std::thread::scope(|threads| {
        let (scope, parked) = (&scope, &mut parked);
        let there = threads.spawn(move || {
            let scope = scope.lock().unwrap();
            let mut cursor = scope.resume(parked).expect("its own scope");
            cursor.next().expect("a step");
            cursor.key().map(<[u8]>::to_vec)
        });
        assert_eq!(there.join().expect("the cursor was moved"), Some(key(12)));
    });
    assert_eq!(db.pool.census().pinned, 0);

    // The scope writes, and the cursor reads its writes beside the Merk of
    // its version.
    let scope = scope.into_inner().unwrap();
    let mine = change(&world, &tx, 1, |items| {
        items.insert(b"0012x".to_vec(), b"written beside the cursor".to_vec());
        items.remove(&key(14));
    });
    apply_through(&world, &tx, &scope, &mine);
    // What is pinned now is pinned by the transaction's frames.
    let frames = db.pool.census().pinned;
    let view = tx.model.borrow().view(&at_open, unit);
    let keys: Vec<Vec<u8>> = view.keys().cloned().collect();
    let mut cursor = scope.resume(&mut parked).expect("its own scope");
    assert_eq!(cursor.version(), opened);
    assert_eq!(cursor.key(), Some(&key(12)[..]), "it kept its place");
    cursor.next().expect("a step");
    assert_eq!(cursor.key(), Some(&b"0012x"[..]));
    assert_eq!(scan_merk(cursor.cursor(), &view, false), keys);
    let mut back = scan_merk(cursor.cursor(), &view, true);
    back.reverse();
    assert_eq!(back, keys);
    assert_eq!(db.pool.census().pinned, frames, "the cursor pins none");

    // The pin of the version goes with the cursor, not with its scope.
    scope.merge();
    assert_eq!(db.oldest_pin(&versions), opened);
    drop(parked);
    assert!(db.oldest_pin(&versions) > opened);
    assert_eq!(db.pool.census().pinned, frames);
    world.check(&tx);
}

/// A parked cursor is its scope's: no other scope is given it, and no
/// scope once the one that opened it has ended. A read through it is a
/// read of its scope, which another scope's write claim refuses (SC3).
#[test]
fn a_parked_cursor_is_resumed_by_the_scope_that_opened_it_and_no_other() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let subtree = model::subtree(1);
    world.edit_outside(
        1,
        |items| {
            for index in 0..10 {
                items.insert(key(index), vec![7; 20]);
            }
        },
        &[],
    );
    world.edit_outside(2, |items| drop(items.insert(key(0), vec![8; 20])), &[]);

    let tx = world.begin();
    let first = tx.txn.scope().expect("a scope");
    let mut parked = first.parked_merk_cursor(subtree).expect("a cursor");
    let mut elsewhere = first
        .parked_merk_cursor(model::subtree(2))
        .expect("a cursor");
    // A scope that is open beside it, and a scope of another transaction.
    let second = tx.txn.scope().expect("a scope beside the first");
    assert!(second.resume(&mut parked).is_none());
    let other = world.begin();
    let theirs = other.txn.scope().expect("a scope");
    assert!(theirs.resume(&mut parked).is_none());
    theirs.rollback();

    // The second scope writes the Merk before the cursor has read it. The
    // cursor's read is refused, and the cursor of another Merk is not.
    let written = change(&world, &tx, 1, |items| {
        drop(items.insert(key(20), b"the second scope's".to_vec()))
    });
    apply_through(&world, &tx, &second, &written);
    let mut cursor = first.resume(&mut parked).expect("its own scope");
    let refused = cursor.seek_to_first().expect_err("the Merk is held");
    assert!(
        matches!(refused, TxnError::ScopeOverlap { unit } if unit == Unit::data(subtree)),
        "{refused:?}"
    );
    assert_eq!(cursor.key(), None);
    let refused = cursor.node(|node| node.is_some());
    assert!(matches!(refused, Ok(false)), "it stands on no key");
    let mut cursor = first.resume(&mut elsewhere).expect("its own scope");
    cursor.seek_to_first().expect("a seek");
    assert_eq!(cursor.key(), Some(&key(0)[..]));

    // The second scope was opened beside the first, so its writes are kept
    // in the write set: its own cursor shows them as the records it wrote,
    // each with the keys of the children the record names.
    let view = (tx.model.borrow()).view(&world.model.records, Unit::data(subtree));
    let keys: Vec<Vec<u8>> = view.keys().cloned().collect();
    assert!(!second.is_laid_out());
    let mut own = second.parked_merk_cursor(subtree).expect("a cursor");
    let mut cursor = second.resume(&mut own).expect("its own scope");
    assert_eq!(scan_merk(cursor.cursor(), &view, false), keys);
    cursor.seek(&key(20)).expect("a seek");
    assert!(cursor
        .node(|node| node.is_some_and(|node| node.is_record()))
        .expect("a read"));
    drop(own);

    // Once the second scope has merged, the first reads what it wrote.
    second.merge();
    let mut cursor = first.resume(&mut parked).expect("its own scope");
    cursor.seek_to_last().expect("a seek");
    assert_eq!(cursor.key(), Some(&key(20)[..]));

    // The scope ends. No scope has its number again: the cursor is given
    // to none, whether the scope merged or was rolled back.
    first.merge();
    let later = tx.txn.scope().expect("a scope");
    assert!(later.resume(&mut parked).is_none());
    assert!(later.resume(&mut elsewhere).is_none());
    let mut rolled = later.parked_merk_cursor(subtree).expect("a cursor");
    assert!(later.resume(&mut rolled).is_some());
    later.rollback();
    let last = tx.txn.scope().expect("a scope");
    assert!(last.resume(&mut rolled).is_none());
    last.merge();
    drop((parked, elsewhere, rolled));
    world.check(&tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
}

/// Nothing of a subtree is read without its live entry (D-090): a parked
/// cursor asks the directory again after its scope dropped or cleared
/// something, as every cursor does. Its committed side is not read from
/// then on, and what the scope writes under the subtree afterwards is.
#[test]
fn a_parked_cursor_asks_for_its_subtree_again_after_a_drop_and_a_clear() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    for name in [1, 2] {
        world.edit_outside(
            name,
            |items| {
                for index in 0..12 {
                    items.insert(key(index), vec![name; 30]);
                }
            },
            &[],
        );
    }
    let tx = world.begin();
    let scope = tx.txn.scope().expect("a scope");
    let mut dropped = scope
        .parked_merk_cursor(model::subtree(1))
        .expect("a cursor");
    let mut cleared = scope
        .parked_merk_cursor(model::subtree(2))
        .expect("a cursor");
    for parked in [&mut dropped, &mut cleared] {
        let mut cursor = scope.resume(parked).expect("its own scope");
        cursor.seek(&key(5)).expect("a seek");
        assert_eq!(cursor.key(), Some(&key(5)[..]));
    }

    // One Merk is dropped, and the other cleared and written anew with
    // two nodes, in one batch each.
    let leaves = OwnedBatch {
        subtrees: vec![OwnedSubtree::Drop(model::subtree(1))],
        ..OwnedBatch::default()
    };
    apply_through(&world, &tx, &scope, &leaves);
    let items: BTreeMap<Vec<u8>, Vec<u8>> = [(key(3), vec![9; 9]), (key(30), vec![9; 9])].into();
    apply_through(&world, &tx, &scope, &anew(model::subtree(2), &items));

    // Each cursor stands on its key, with no node under it.
    let mut cursor = scope.resume(&mut dropped).expect("its own scope");
    assert_eq!(cursor.key(), Some(&key(5)[..]));
    assert!(cursor.node(|node| node.is_none()).expect("a read"));
    cursor.next().expect("a step");
    assert_eq!(cursor.key(), None, "nothing committed is read");
    cursor.seek_to_first().expect("a seek");
    assert_eq!(cursor.key(), None);

    let mut cursor = scope.resume(&mut cleared).expect("its own scope");
    let mut children = ChildKeys::new();
    let none = cursor.node_with_children(&mut children, |node| node.is_none());
    assert!(none.expect("a read"));
    cursor.next().expect("a step");
    assert_eq!(cursor.key(), Some(&key(30)[..]), "the scope's own node");
    cursor.next().expect("a step");
    assert_eq!(cursor.key(), None);
    let view = tx
        .model
        .borrow()
        .view(&world.model.records, Unit::data(model::subtree(2)));
    assert_eq!(scan_merk(cursor.cursor(), &view, false), [key(3), key(30)]);

    scope.merge();
    drop((dropped, cleared));
    world.check(&tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
}

/// The Merks of two subtrees one inside the other, each with its change
/// set, as one batch: the node `host` of the outer one hosts the inner
/// one. With `enter` the batch enters both subtrees first.
fn nested<'s>(
    world: &World<'s>,
    tx: &Tx<'s>,
    (outer, inner): (u8, u8),
    enter: bool,
    edit: impl FnOnce(&mut BTreeMap<Vec<u8>, Vec<u8>>),
) -> OwnedBatch {
    let below = world.merk(tx, inner, edit);
    let stamp = below.nodes.len() as u8;
    let mut above = world.merk(tx, outer, |items| {
        let mut value = items.get(&b"host"[..]).cloned().unwrap_or_default();
        value.push(stamp);
        items.insert(b"host".to_vec(), value);
    });
    (above.hosted).insert(b"host".to_vec(), vec![model::subtree(inner)]);
    let host = (model::subtree(outer), b"host".to_vec(), 0);
    let subtrees = match enter {
        true => vec![
            OwnedSubtree::Enter(model::subtree(outer), None),
            OwnedSubtree::Enter(model::subtree(inner), Some(host)),
        ],
        false => Vec::new(),
    };
    OwnedBatch {
        merks: vec![below, above],
        subtrees,
        ..OwnedBatch::default()
    }
}

/// A cursor that stands on a key its transaction wrote and took back reads
/// the node of its own version there, found by the key, and the keys of
/// that node's children from their cells at that version.
///
/// The cursor's committed side is on no key throughout. The transaction
/// drops the subtree above the cursor's, under which no committed node is
/// read (CV9), enters both again and writes the Merk anew: the cursor
/// comes to each key over the transaction's nodes alone. After the
/// rollback the node under the key is a committed one that the committed
/// side does not stand on, so it is found by a descent from the root.
#[test]
fn the_children_of_a_node_that_is_found_by_its_key_are_read_at_the_cursors_version() {
    use super::store::ENTERED;

    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let names = (ENTERED + 20, ENTERED + 21);
    let subtree = model::subtree(names.1);
    let unit = Unit::data(subtree);
    let fill = |items: &mut BTreeMap<Vec<u8>, Vec<u8>>| {
        for index in 0..14 {
            items.insert(key(index), vec![index as u8; 25]);
        }
    };
    let tx = world.begin();
    let planted = nested(&world, &tx, names, true, fill);
    world.apply(&tx, &planted).expect("the subtrees are made");
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));

    let tx = world.begin();
    let view = tx.model.borrow().view(&world.model.records, unit);
    let items = Committed::items(&view);
    let mut cursor = tx.txn.merk_cursor(subtree).expect("a cursor");
    let opened = cursor.version();
    // A commit outside gives the Merk another shape after the cursor was
    // opened: the cursor's version is not the latest one.
    let other = world.begin();
    let reshaped = nested(&world, &other, names, false, |items| {
        for index in 0..14 {
            match index % 3 {
                0 => drop(items.remove(&key(index))),
                _ => drop(items.insert(key(index + 100), vec![1; 5])),
            }
        }
    });
    world.apply(&other, &reshaped).expect("the Merk is changed");
    assert!(matches!(world.commit(other, &[&tx]), Outcome::Committed(_)));
    assert!(world.latest() > opened);

    let mut with_children = 0;
    for (at, expected) in &view {
        let Val::Node(expected) = expected else {
            unreachable!("a Merk");
        };
        let mark = tx.txn.mark();
        let dropped = OwnedBatch {
            subtrees: vec![OwnedSubtree::Drop(model::subtree(names.0))],
            ..OwnedBatch::default()
        };
        world
            .apply(&tx, &dropped)
            .expect("the subtree above is dropped");
        let again = nested(&world, &tx, names, true, |anew| *anew = items.clone());
        world
            .apply(&tx, &again)
            .expect("both are entered and written again");
        cursor.seek(at).expect("a seek");
        assert_eq!(cursor.key(), Some(&at[..]));
        check_under(&mut cursor, at, expected);

        // The writes are taken back. The cursor stands on the key, and the
        // node under it is the one of the cursor's version, with the
        // children it has there.
        assert!(tx.txn.rewind(mark));
        tx.forget();
        assert_eq!(cursor.key(), Some(&at[..]));
        check_under(&mut cursor, at, expected);
        with_children += usize::from(expected.left.is_some() || expected.right.is_some());
    }
    assert!(with_children > 3, "nodes with children were read");
    assert_eq!(cursor.version(), opened);
    drop(cursor);
}

/// What a walk asks of the buffer pool, for PERF-01 and PERF-03: a step, a
/// step with the node read, and a step with the node and the keys of its
/// children. Each request is one lookup in the pool, and a load only where
/// the page is not there.
///
/// The keys of a node's children are read from the children's cells: at
/// most two requests more for a node, and none for a leaf.
#[test]
fn a_node_with_the_keys_of_its_children_asks_the_pool_for_at_most_two_pages_more() {
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(5);
    let subtree = model::subtree(1);
    let nodes = 2_000;
    world.edit_outside(
        1,
        |items| {
            for index in 0..nodes {
                items.insert(key(index), value(&mut rng, 80));
            }
        },
        &[],
    );
    let tx = world.begin();
    let view = (tx.model.borrow()).view(&world.model.records, Unit::data(subtree));
    let (mut children_of, mut leaves) = (0, 0);
    for node in view.values() {
        let Val::Node(node) = node else {
            unreachable!("a Merk");
        };
        let children = usize::from(node.left.is_some()) + usize::from(node.right.is_some());
        children_of += children;
        leaves += usize::from(children == 0);
    }
    assert_eq!(children_of, nodes - 1, "every node but the root is a child");

    let requests = || {
        let stats = db.pool.stats();
        stats.hits + stats.misses
    };
    #[derive(Clone, Copy)]
    enum Reads {
        Keys,
        Nodes,
        Children,
    }
    let mut cursor = tx.txn.merk_cursor(subtree).expect("a cursor");
    let mut walk = |reads: Reads| {
        let before = requests();
        let mut children = ChildKeys::new();
        let mut seen = 0;
        cursor.seek_to_first().expect("a seek");
        while cursor.valid() {
            match reads {
                Reads::Keys => {}
                Reads::Nodes => cursor.node(|node| assert!(node.is_some())).expect("a read"),
                Reads::Children => cursor
                    .node_with_children(&mut children, |node| assert!(node.is_some()))
                    .expect("a read"),
            }
            seen += 1;
            cursor.next().expect("a step");
        }
        assert_eq!(seen, nodes);
        (requests() - before) as usize
    };
    let (keys, with_nodes, with_children) =
        (walk(Reads::Keys), walk(Reads::Nodes), walk(Reads::Children));
    println!(
        "pool requests over {nodes} nodes ({leaves} leaves): {keys} for the steps, \
         {} more with each node read, {} more with the keys of its children",
        with_nodes - keys,
        with_children - with_nodes,
    );
    // TREE-06: a step asks for one to three pages.
    assert!(keys >= nodes && keys <= 3 * nodes + 64, "{keys}");
    // The node under the cursor is one request.
    assert_eq!(with_nodes - keys, nodes);
    // And one for each child there is: two at most for a node.
    assert_eq!(with_children - with_nodes, children_of);
}

/// One cursor reads several Merks as one committed version has them
/// ([`MerkCursor::retarget`]): pointed at another Merk it keeps its
/// version and its pin, stands on no key until it is sought, and shows that
/// Merk as the version has it beside the transaction's writes to it. So
/// does a parked cursor, through its scope.
#[test]
fn a_cursor_is_pointed_at_another_merk_of_the_version_it_pinned() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let unit = |name: u8| Unit::data(model::subtree(name));
    for name in [1u8, 2, 3] {
        world.edit_outside(
            name,
            |items| {
                for index in 0..usize::from(name) * 9 {
                    items.insert(key(index * 2), vec![name; 40]);
                }
            },
            &[],
        );
    }
    let at_open = world.model.records.clone();
    let opened = world.latest();
    let keys_at_open = |name: u8| -> Vec<Vec<u8>> {
        let of_unit = at_open.iter().filter(|((own, _), _)| *own == unit(name));
        of_unit.map(|((_, key), _)| key.clone()).collect()
    };

    let tx = world.begin();
    let mut cursor = tx.txn.merk_cursor(model::subtree(1)).expect("a cursor");
    cursor.seek(&key(4)).expect("a seek");
    assert_eq!(cursor.key(), Some(&key(4)[..]));
    let pins_at_open = versions.pins_held();
    assert!(pins_at_open > 0, "the cursor holds a pin");

    // Commits outside, after the cursor was opened: every Merk gets a
    // node, loses one, and the third is made again whole, which frees the
    // pages the version has of it.
    for name in [1u8, 2, 3] {
        world.edit_outside(
            name,
            |items| {
                items.insert(key(1), b"outside".to_vec());
                items.remove(&key(2));
            },
            &[&tx],
        );
    }
    world.edit_outside(
        3,
        |items| {
            items.clear();
            for index in 0..50 {
                items.insert(key(index), vec![0xee; 70]);
            }
        },
        &[&tx],
    );
    assert!(world.latest() > opened);

    // Pointed at the second Merk it stands on no key, and a step from
    // there goes nowhere. Sought, it shows the Merk of its version.
    cursor
        .retarget(model::subtree(2))
        .expect("the directory is read");
    assert_eq!(cursor.version(), opened);
    assert!(!cursor.valid());
    assert!(cursor.node(|node| node.is_none()).expect("a read"));
    cursor.next().expect("a step from no key");
    cursor.prev().expect("a step from no key");
    assert_eq!(cursor.key(), None);
    for name in [2u8, 3, 1, 3] {
        cursor.retarget(model::subtree(name)).expect("a retarget");
        let view = tx.model.borrow().view(&at_open, unit(name));
        assert_eq!(scan_merk(&mut cursor, &view, false), keys_at_open(name));
        let mut back = scan_merk(&mut cursor, &view, true);
        back.reverse();
        assert_eq!(back, keys_at_open(name), "Merk {name}");
        // It is left standing on a key, and the next retarget takes it
        // from there.
        cursor.seek_for_prev(&key(5)).expect("a seek");
        assert_eq!(cursor.key(), Some(&key(4)[..]));
    }
    assert_eq!(
        db.oldest_pin(&versions),
        opened,
        "the version it was opened on"
    );
    assert_eq!(versions.pins_held(), pins_at_open, "and no pin more");
    assert_eq!(db.pool.census().pinned, 0, "and no page");

    // A subtree the version has no entry for has nothing to show, and the
    // cursor goes on from there.
    cursor.retarget(model::subtree(9)).expect("a retarget");
    cursor.seek_to_first().expect("a seek");
    assert_eq!(cursor.key(), None);
    cursor.seek_for_prev(&key(99)).expect("a seek");
    assert_eq!(cursor.key(), None);

    // The transaction writes the second Merk. The cursor, pointed there,
    // shows the write beside the nodes of its version, and pointed back at
    // the first Merk it shows none of it.
    let scope = tx.txn.scope().expect("a scope");
    let mine = change(&world, &tx, 2, |items| {
        items.insert(b"0003x".to_vec(), b"written by the transaction".to_vec());
        items.remove(&key(6));
    });
    // The transaction's own change is made from the latest state, which
    // has the commits from outside: its view is over that.
    apply_through(&world, &tx, &scope, &mine);
    scope.merge();
    cursor.retarget(model::subtree(2)).expect("a retarget");
    cursor.seek(b"0003").expect("a seek");
    assert_eq!(cursor.key(), Some(&b"0003x"[..]), "its transaction's node");
    cursor.seek(&key(6)).expect("a seek");
    assert_eq!(cursor.key(), Some(&key(8)[..]), "its transaction's removal");
    cursor.retarget(model::subtree(1)).expect("a retarget");
    let view = tx.model.borrow().view(&at_open, unit(1));
    assert_eq!(scan_merk(&mut cursor, &view, false), keys_at_open(1));
    drop(cursor);
    assert!(db.oldest_pin(&versions) > opened, "the pin went with it");

    // A parked cursor is pointed elsewhere through its scope, and a read
    // of the Merk it is pointed at is a read of that Merk: another scope
    // that holds it for writing refuses it (SC3).
    let now = world.model.records.clone();
    let first = tx.txn.scope().expect("a scope");
    let mut parked = first
        .parked_merk_cursor(model::subtree(1))
        .expect("a cursor");
    let second = tx.txn.scope().expect("a scope beside the first");
    let theirs = change(&world, &tx, 3, |items| {
        drop(items.insert(b"zz".to_vec(), b"the second scope's".to_vec()))
    });
    apply_through(&world, &tx, &second, &theirs);
    {
        let mut cursor = first.resume(&mut parked).expect("its own scope");
        cursor.seek_to_first().expect("the first Merk is free");
        assert!(cursor.valid());
        cursor
            .retarget(model::subtree(3))
            .expect("a retarget reads no node");
        assert!(!cursor.valid());
        let refused = cursor.seek_to_first().expect_err("the Merk is held");
        assert!(
            matches!(refused, TxnError::ScopeOverlap { unit: held } if held == unit(3)),
            "{refused:?}"
        );
        assert_eq!(cursor.key(), None);
        // The refusal is of that Merk, and leaves the cursor usable.
        cursor.retarget(model::subtree(1)).expect("a retarget");
        cursor.seek_to_last().expect("a seek");
        assert!(cursor.valid());
    }
    second.merge();
    {
        let mut cursor = first.resume(&mut parked).expect("its own scope");
        cursor.retarget(model::subtree(3)).expect("a retarget");
        cursor.seek_to_last().expect("the Merk is free again");
        assert_eq!(cursor.key(), Some(&b"zz"[..]));
        let view = tx.model.borrow().view(&now, unit(3));
        let keys: Vec<Vec<u8>> = view.keys().cloned().collect();
        assert_eq!(scan_merk(cursor.cursor(), &view, false), keys);
    }
    first.merge();
    drop(parked);
    world.check(&tx);
}
