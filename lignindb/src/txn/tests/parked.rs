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
        cursor.seek(&key(10)).expect("a seek");
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

/// A cursor that stands on a key its transaction wrote and took back reads
/// the node of its own version there, found by the key, and the keys of
/// that node's children from their cells at that version.
#[test]
fn the_children_of_a_node_that_is_found_by_its_key_are_read_at_the_cursors_version() {
    let (db, versions) = Db::new(64);
    let mut world = World::new(&db, &versions);
    let subtree = model::subtree(2);
    let unit = Unit::data(subtree);
    world.edit_outside(
        2,
        |items| {
            for index in 0..14 {
                items.insert(key(index), vec![index as u8; 25]);
            }
        },
        &[],
    );
    let tx = world.begin();
    let view = tx.model.borrow().view(&world.model.records, unit);
    let items = Committed::items(&view);
    let mut cursor = tx.txn.merk_cursor(subtree).expect("a cursor");
    // A commit outside gives the Merk another shape after the cursor was
    // opened: the cursor's version is not the latest one.
    world.edit_outside(
        2,
        |items| {
            for index in 0..14 {
                match index % 3 {
                    0 => drop(items.remove(&key(index))),
                    _ => drop(items.insert(key(index + 100), vec![1; 5])),
                }
            }
        },
        &[&tx],
    );
    let mut with_children = 0;
    for (at, expected) in &view {
        let Val::Node(expected) = expected else {
            unreachable!("a Merk");
        };
        // The transaction clears the Merk and writes it anew. The cursor
        // comes to the key over the transaction's nodes alone: its
        // committed side is not read, and stays where it was.
        let mark = tx.txn.mark();
        (world.apply(&tx, &anew(subtree, &items))).expect("the Merk is written anew");
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
    drop(cursor);
    world.check(&tx);
    world.check_committed();
}
