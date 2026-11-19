//! The read rule of a write transaction (`transactions.md` RD3 to RD6,
//! RD10): the cases of section 16 that are about reads (T-RD2, T-RD3), the
//! committed read (T-RD8), and the cursor.

use super::{
    model::{self, value, OwnedBatch},
    store::{Db, Outcome, Rng},
    *,
};
use crate::snapshot::SnapshotError;

fn aux() -> Unit {
    model::unit(Column::Aux, 0)
}

fn get(tx: &Tx<'_>, unit: Unit, key: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let found = tx.txn.get(unit, key, &mut out).expect("a read");
    found.then_some(out)
}

fn node_value(tx: &Tx<'_>, name: u8, key: &[u8]) -> Option<Vec<u8>> {
    let read = |view: Option<NodeView<'_>>| view.map(|view| value_of(&view));
    (tx.txn.node(model::subtree(name), key, read)).expect("a read")
}

#[test]
fn a_read_sees_a_commit_made_outside_unless_the_transaction_wrote_the_key() {
    let (db, versions) = Db::new(48);
    let mut world = World::new(&db, &versions);
    let batch = |records| OwnedBatch {
        merks: Vec::new(),
        records,
        ..OwnedBatch::default()
    };

    // A3: T puts K1; W puts K2; T reads K2 and commits.
    let tx = world.begin();
    world.write(&tx, vec![put(aux(), b"k1", b"t")]);
    world.write_outside(&batch(vec![put(aux(), b"k2", b"w")]), &[&tx]);
    assert_eq!(get(&tx, aux(), b"k2"), Some(b"w".to_vec()));
    assert_eq!(get(&tx, aux(), b"k1"), Some(b"t".to_vec()));
    world.check(&tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));

    // A3b: T reads K; W puts K; T reads K again and sees it. Reads are not
    // tracked, so T commits.
    let tx = world.begin();
    assert_eq!(get(&tx, aux(), b"k3"), None);
    world.write_outside(&batch(vec![put(aux(), b"k3", b"w")]), &[&tx]);
    assert_eq!(get(&tx, aux(), b"k3"), Some(b"w".to_vec()));
    world.write(&tx, vec![put(aux(), b"k4", b"t")]);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));

    // A3c: T puts K; W puts K; T reads its own value.
    let tx = world.begin();
    world.write(&tx, vec![put(aux(), b"k5", b"t")]);
    world.write_outside(&batch(vec![put(aux(), b"k5", b"w")]), &[&tx]);
    assert_eq!(get(&tx, aux(), b"k5"), Some(b"t".to_vec()));
    // The committed read gives what the commit outside made of it (RD10).
    let mut out = Vec::new();
    assert!(tx
        .txn
        .committed_get(aux(), b"k5", &mut out)
        .expect("a read"));
    assert_eq!(out, b"w");
    world.check(&tx);
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);

    // A3d: T deletes K; W puts K; T reads no record.
    let tx = world.begin();
    world.write(&tx, vec![delete(aux(), b"k6")]);
    world.write_outside(&batch(vec![put(aux(), b"k6", b"w")]), &[&tx]);
    assert_eq!(get(&tx, aux(), b"k6"), None);
    world.check(&tx);
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);
    world.check_committed();
}

#[test]
fn the_nodes_of_a_merk_are_read_by_the_same_rule() {
    let (db, versions) = Db::new(48);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(3);
    for name in [1, 2] {
        world.edit_outside(
            name,
            |items| {
                for index in 0..30u8 {
                    items.insert(vec![b'a', index], value(&mut rng, 80));
                }
            },
            &[],
        );
    }
    // T writes one node of Merk 1 and reads Merk 2, which it has not
    // written.
    let tx = world.begin();
    world.edit(&tx, 1, |items| {
        items.insert(vec![b'a', 3], b"mine".to_vec());
    });
    assert_eq!(node_value(&tx, 1, &[b'a', 3]), Some(b"mine".to_vec()));
    // A commit outside changes a node of Merk 2: T sees it at once.
    world.edit_outside(
        2,
        |items| *items.get_mut(&vec![b'a', 7]).unwrap() = b"theirs".to_vec(),
        &[&tx],
    );
    assert_eq!(node_value(&tx, 2, &[b'a', 7]), Some(b"theirs".to_vec()));
    assert_eq!(
        tx.txn.stats().overlapped,
        0,
        "the two wrote different Merks"
    );
    world.check(&tx);
    // And one that changes Merk 1: T keeps the nodes it wrote, among them
    // every node on the way to the root, and reads the others as committed.
    world.edit_outside(
        1,
        |items| {
            items.insert(vec![b'a', 3], b"theirs".to_vec());
            items.insert(vec![b'z'], b"a new node".to_vec());
        },
        &[&tx],
    );
    assert_eq!(node_value(&tx, 1, &[b'a', 3]), Some(b"mine".to_vec()));
    assert_eq!(node_value(&tx, 1, b"z"), Some(b"a new node".to_vec()));
    let stats = tx.txn.stats();
    assert!(stats.overlapped > 0);
    assert_eq!(
        (stats.base, stats.pinned),
        (None, None),
        "it serves its records by key"
    );
    // The committed read of the node T wrote is the newer committed node.
    let committed = |view: Option<NodeView<'_>>| view.map(|view| view.value_len());
    let len = (tx
        .txn
        .committed_node(model::subtree(1), &[b'a', 3], committed))
    .expect("a read");
    assert_eq!(len, Some(6));
    world.check(&tx);
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);
    world.check_committed();
}

/// T-RB1, case B16: the transaction has written `x`, and a write with no
/// transaction commits `y`, which lives on the same flat page.
#[test]
fn a_transaction_keeps_its_key_beside_one_committed_on_the_same_page() {
    let (db, versions) = Db::new(48);
    let mut world = World::new(&db, &versions);
    let tx = world.begin();
    world.write(&tx, vec![put(aux(), b"x", b"the transaction's")]);
    let base = tx.txn.stats().base;
    let records = vec![put(aux(), b"y", b"committed outside")];
    let merks = Vec::new();
    world.write_outside(
        &OwnedBatch {
            merks,
            records,
            ..OwnedBatch::default()
        },
        &[&tx],
    );

    assert_eq!(get(&tx, aux(), b"y"), Some(b"committed outside".to_vec()));
    assert_eq!(get(&tx, aux(), b"x"), Some(b"the transaction's".to_vec()));
    let stats = tx.txn.stats();
    assert_eq!((stats.overlapped, stats.rebuilds), (0, 1), "it was rebased");
    assert_eq!(stats.base, Some(world.latest()));
    assert_ne!(stats.base, base);
    assert_eq!(
        stats.flat_pages, 1,
        "both keys are in the one page it copied"
    );
    world.check(&tx);

    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    let reader = world.begin();
    assert_eq!(
        get(&reader, aux(), b"x"),
        Some(b"the transaction's".to_vec())
    );
    assert_eq!(
        get(&reader, aux(), b"y"),
        Some(b"committed outside".to_vec())
    );
    world.check_committed();
}

/// Cases A12 and A12b (T-RD3).
#[test]
fn a_cursor_reads_the_state_it_was_opened_on_and_the_transactions_later_writes() {
    let (db, versions) = Db::new(48);
    let mut world = World::new(&db, &versions);
    let batch = |records| OwnedBatch {
        records,
        ..OwnedBatch::default()
    };
    world.write_outside(
        &batch(vec![put(aux(), b"k1", b"1"), put(aux(), b"k3", b"3")]),
        &[],
    );
    let keys = |records: Vec<(Vec<u8>, Vec<u8>)>| -> Vec<Vec<u8>> {
        records.into_iter().map(|(key, _)| key).collect()
    };

    for writes in [false, true] {
        let tx = world.begin();
        if writes {
            world.write(&tx, vec![put(aux(), b"k0", b"0")]);
        }
        let opened = world.latest();
        let mut first = tx.txn.cursor(aux());
        assert_eq!(first.version(), opened);
        assert!(!first.valid());
        // A12: a commit outside, after the cursor was opened.
        world.write_outside(&batch(vec![put(aux(), b"k2", b"2")]), &[&tx]);
        let seen = keys(scan(&mut first, false));
        assert!(
            !seen.contains(&b"k2".to_vec()),
            "the first cursor: {seen:?}"
        );
        assert!(seen.contains(&b"k1".to_vec()) && seen.contains(&b"k3".to_vec()));
        let mut second = tx.txn.cursor(aux());
        assert!(keys(scan(&mut second, false)).contains(&b"k2".to_vec()));
        // A12b: a write of the transaction, after the cursor was opened.
        world.write(&tx, vec![put(aux(), b"k9", b"9"), delete(aux(), b"k1")]);
        let seen = keys(scan(&mut first, false));
        assert!(seen.contains(&b"k9".to_vec()) && !seen.contains(&b"k1".to_vec()));
        assert!(!seen.contains(&b"k2".to_vec()));
        assert_eq!(seen.contains(&b"k0".to_vec()), writes);
        drop((first, second));
        world.write_outside(&batch(vec![delete(aux(), b"k2")]), &[&tx]);
        tx.txn.rollback().expect("no scope is open");
    }
}

#[test]
fn a_cursor_sees_the_transactions_writes_in_key_order() {
    let (db, versions) = Db::new(64);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(11);
    let unit = model::unit(Column::Meta, 4);
    let key = |index: usize| format!("{index:05}").into_bytes();
    let mut records = Vec::new();
    for index in 0..300 {
        let len = rng.below(60);
        records.push(put(unit, &key(index * 3), &value(&mut rng, len)));
    }
    let merks = Vec::new();
    world.write_outside(
        &OwnedBatch {
            merks,
            records,
            ..OwnedBatch::default()
        },
        &[],
    );

    let tx = world.begin();
    // Puts between the committed keys and over them, deletes, and ranges
    // that hide committed records and the transaction's earlier puts.
    for round in 0..6 {
        let mut records = Vec::new();
        let mut written = BTreeSet::new();
        for _ in 0..40 {
            let index = rng.below(950);
            if !written.insert(index) {
                continue;
            }
            records.push(match rng.below(10) {
                0..=5 => put(unit, &key(index), &value(&mut rng, 30)),
                6..=8 => delete(unit, &key(index)),
                _ => Write::Range(unit, key(index), Some(key(index + rng.below(25)))),
            });
        }
        world.write(&tx, records);
        if round == 2 {
            world.write(&tx, vec![Write::Range(unit, key(880), None)]);
        }
        world.check(&tx);
    }
    let view: Vec<(Vec<u8>, Vec<u8>)> = (tx
        .model
        .borrow()
        .view(&world.model.records, unit)
        .into_iter())
    .map(|(key, value)| match value {
        Val::Flat(value) => (key, value),
        Val::Node(_) => unreachable!(),
    })
    .collect();
    assert!(view.len() > 200);
    let expected = |at: Option<usize>| at.map(|at| view[at].clone());
    let shown = |cursor: &mut Cursor<'_, '_>| {
        let key = cursor.key().map(<[u8]>::to_vec)?;
        let mut value = Vec::new();
        assert!(cursor.value(&mut value).expect("a value"));
        Some((key, value))
    };

    // Seeks to any key, and steps both ways from there, with turns.
    let mut cursor = tx.txn.cursor(unit);
    for _ in 0..300 {
        let target = key(rng.below(1000));
        let mut at = if rng.chance(50) {
            cursor.seek(&target).expect("a seek");
            view.iter().position(|(key, _)| *key >= target)
        } else {
            cursor.seek_for_prev(&target).expect("a seek");
            view.iter().rposition(|(key, _)| *key <= target)
        };
        assert_eq!(shown(&mut cursor), expected(at), "a seek to {target:?}");
        for _ in 0..rng.below(8) {
            if rng.chance(60) {
                cursor.next().expect("a step");
                at = at.and_then(|at| (at + 1 < view.len()).then_some(at + 1));
            } else {
                cursor.prev().expect("a step");
                at = at.and_then(|at| at.checked_sub(1));
            }
            assert_eq!(shown(&mut cursor), expected(at));
        }
    }
    // From no key a step goes nowhere.
    cursor.seek(b"99999").expect("a seek");
    assert!(!cursor.valid());
    cursor.next().expect("a step");
    cursor.prev().expect("a step");
    assert!(!cursor.valid());
    let mut none = Vec::new();
    assert!(!cursor.value(&mut none).expect("no value"));
    assert!(format!("{cursor:?}").contains("Cursor"));

    // The one pattern of writing under a cursor that the seam defines:
    // delete the record it stands on, and step (flat-store.md rule 7).
    cursor.seek_to_first().expect("a seek");
    let mut left = view.len();
    while let Some(at) = cursor.key().map(<[u8]>::to_vec) {
        world.write(&tx, vec![delete(unit, &at)]);
        assert_eq!(cursor.key(), Some(&at[..]), "it stands on the key");
        assert!(
            !cursor.value(&mut none).expect("a read"),
            "the record is gone"
        );
        cursor.next().expect("a step");
        left -= 1;
    }
    assert_eq!(left, 0);
    drop(cursor);
    assert!(scan(&mut tx.txn.cursor(unit), false).is_empty());
    world.check(&tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
}

/// A thread that walks a cursor and writes as it goes does both through
/// its scope: the cursor is the scope's, and stays open across the write
/// (RD6, SC4).
#[test]
fn a_scope_writes_under_its_own_cursor() {
    let (db, versions) = Db::new(32);
    let mut world = World::new(&db, &versions);
    let unit = aux();
    let committed = [b"a", b"c", b"e", b"g"].map(|key| put(unit, key, b"committed"));
    let batch = OwnedBatch {
        merks: Vec::new(),
        records: committed.to_vec(),
        ..OwnedBatch::default()
    };
    world.write_outside(&batch, &[]);

    let tx = world.begin();
    let scope = tx.txn.scope().expect("no scope is open");
    let mut cursor = scope.cursor(unit);
    cursor.seek_to_first().expect("a seek");
    assert_eq!(cursor.key(), Some(&b"a"[..]));
    // Ahead of the cursor: a new record, a delete, and a new value.
    let writes = [
        RecordWrite::put(Column::Aux, unit.subtree, b"b", b"mine"),
        RecordWrite::delete(Column::Aux, unit.subtree, b"c"),
        RecordWrite::put(Column::Aux, unit.subtree, b"e", b"mine"),
    ];
    let batch = Batch {
        records: &writes,
        merks: &[],
        ..Batch::default()
    };
    let first_write = world.latest().get();
    scope.apply(&batch).expect("a write under the cursor");
    let mut shown = Vec::new();
    loop {
        cursor.next().expect("a step");
        let Some(key) = cursor.key().map(<[u8]>::to_vec) else {
            break;
        };
        let mut value = Vec::new();
        assert!(cursor.value(&mut value).expect("a value"));
        shown.push((key, value));
    }
    let expected = [
        (b"b".to_vec(), b"mine".to_vec()),
        (b"e".to_vec(), b"mine".to_vec()),
        (b"g".to_vec(), b"committed".to_vec()),
    ];
    assert_eq!(shown, expected);
    drop(cursor);
    scope.merge();
    let writes = [
        put(unit, b"b", b"mine"),
        delete(unit, b"c"),
        put(unit, b"e", b"mine"),
    ];
    (tx.model.borrow_mut().writes).extend(writes.map(|write| (write, first_write, false)));
    world.check(&tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
}

/// RD6 for the nodes of a Merk: the cursor reads the Merk as the version
/// of its opening has it, merged in key order with the transaction's nodes
/// as they are at each step. Commits outside that free and reuse the pages
/// under it change nothing it shows (T-RD7).
#[test]
fn a_merk_cursor_reads_its_version_and_the_transactions_nodes() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(7);
    let key = |index: usize| format!("{index:04}").into_bytes();
    let subtree = model::subtree(1);
    let unit = Unit::data(subtree);
    world.edit_outside(
        1,
        |items| {
            for index in 0..60 {
                items.insert(key(index * 2), value(&mut rng, 60));
            }
        },
        &[],
    );

    // The transaction writes nodes before, between and after the committed
    // ones, gives one another value and deletes two.
    let tx = world.begin();
    world.edit(&tx, 1, |items| {
        items.insert(b"/first".to_vec(), b"before every committed key".to_vec());
        items.insert(key(3), b"between two committed keys".to_vec());
        items.insert(key(5), b"and again".to_vec());
        items.insert(key(10), b"another value".to_vec());
        items.remove(&key(12));
        items.remove(&key(14));
        items.insert(b"zz".to_vec(), b"after every committed key".to_vec());
    });
    let view = tx.model.borrow().view(&world.model.records, unit);
    let keys: Vec<Vec<u8>> = view.keys().cloned().collect();
    assert_eq!(keys.len(), 60 + 4 - 2);

    let mut cursor = tx.txn.merk_cursor(subtree).expect("a cursor");
    let opened = cursor.version();
    assert_eq!(opened, world.latest());
    assert!(!cursor.valid());
    assert!(cursor.node(|node| node.is_none()).expect("no node"));
    assert!(format!("{cursor:?}").contains("MerkSide"));
    let shown = |cursor: &mut MerkCursor<'_, '_>, at: Option<usize>| {
        assert_eq!(cursor.key(), at.map(|at| &keys[at][..]));
        let Some(at) = at else {
            return;
        };
        let Some(Val::Node(expected)) = view.get(&keys[at]) else {
            unreachable!("a Merk");
        };
        (cursor.node(|node| expected.check(&keys[at], &node.expect("a node")))).expect("a read");
    };
    for _ in 0..300 {
        let target = match rng.below(3) {
            0 => key(rng.below(125)),
            1 => keys[rng.below(keys.len())].clone(),
            _ => vec![b'/' + rng.below(4) as u8, b'0' + rng.below(10) as u8],
        };
        let mut at = if rng.chance(50) {
            cursor.seek(&target).expect("a seek");
            keys.iter().position(|key| *key >= target)
        } else {
            cursor.seek_for_prev(&target).expect("a seek");
            keys.iter().rposition(|key| *key <= target)
        };
        shown(&mut cursor, at);
        for _ in 0..rng.below(8) {
            if rng.chance(60) {
                cursor.next().expect("a step");
                at = at.and_then(|at| (at + 1 < keys.len()).then_some(at + 1));
            } else {
                cursor.prev().expect("a step");
                at = at.and_then(|at| at.checked_sub(1));
            }
            shown(&mut cursor, at);
        }
    }
    // The cursor of another transaction sees the committed Merk.
    let other = world.begin();
    let committed = other.model.borrow().view(&world.model.records, unit);
    let mut theirs = other.txn.merk_cursor(subtree).expect("a cursor");
    assert_eq!(scan_merk(&mut theirs, &committed, false).len(), 60);
    drop(theirs);

    // Commits outside, after the cursor was opened: new nodes, another
    // value, a delete, and then the whole Merk made again four times, which
    // frees its pages and makes others.
    let at_open = world.model.records.clone();
    world.edit_outside(
        1,
        |items| {
            items.insert(key(7), b"outside".to_vec());
            items.insert(key(20), b"outside".to_vec());
            items.remove(&key(30));
        },
        &[&tx],
    );
    for round in 0..4u8 {
        world.edit_outside(
            1,
            |items| {
                items.clear();
                for index in 0..90 {
                    items.insert(key(index), vec![round; 100]);
                }
            },
            &[&tx],
        );
    }
    assert!(world.latest() > opened && cursor.version() == opened);
    // The cursor still shows the Merk of its opening under the keys the
    // transaction has not written, and the transaction's nodes.
    let expected = tx.model.borrow().view(&at_open, unit);
    assert_eq!(scan_merk(&mut cursor, &expected, false), keys);
    let mut back = scan_merk(&mut cursor, &expected, true);
    back.reverse();
    assert_eq!(back, keys);
    assert!(
        tx.txn.stats().overlapped > 0,
        "it serves its records by key"
    );
    // A cursor that is opened now reads the latest version.
    world.check(&tx);

    // A write under the open cursor is seen when the cursor comes to it.
    cursor.seek(&key(40)).expect("a seek");
    assert_eq!(cursor.key(), Some(&key(40)[..]));
    world.edit(&tx, 1, |items| {
        items.insert(key(41), b"written under the cursor".to_vec());
        items.remove(&key(42));
    });
    let value = |node: Option<NodeView<'_>>| node.map(|node| value_of(&node));
    cursor.next().expect("a step");
    assert_eq!(cursor.key(), Some(&key(41)[..]));
    assert_eq!(
        cursor.node(value).expect("a read"),
        Some(b"written under the cursor".to_vec())
    );
    // The node it deleted is gone. What comes next is the next key of the
    // cursor's version or of the transaction's writes: the change set
    // wrote more nodes than the one it added, those whose links changed.
    let now = tx.model.borrow().view(&at_open, unit);
    assert!(!now.contains_key(&key(42)));
    let after = now.keys().find(|at| **at > key(41)).expect("more nodes");
    cursor.next().expect("a step");
    assert_eq!(cursor.key(), Some(&after[..]), "the deleted node is gone");
    let mut all = scan_merk(&mut cursor, &now, true);
    all.reverse();
    assert_eq!(all, now.keys().cloned().collect::<Vec<_>>());
    // The node it stands on is deleted, and then everything is rolled
    // back: the cursor stands on its key and reads what is under it now.
    cursor.seek(&key(44)).expect("a seek");
    assert_eq!(cursor.key(), Some(&key(44)[..]));
    world.edit(&tx, 1, |items| drop(items.remove(&key(44))));
    assert_eq!(cursor.node(value).expect("a read"), None);
    tx.txn.rollback().expect("no scope is open");
    tx.forget();
    let at_44 = cursor.node(value).expect("a read");
    assert_eq!(
        at_44.map(|value| value.len()),
        Some(60),
        "the version's node"
    );
    cursor.prev().expect("a step");
    assert_eq!(cursor.key(), Some(&key(42)[..]), "it is there again");
    drop(cursor);

    // Through a scope, and beside it: both read the Merk (SC3).
    let scope = tx.txn.scope().expect("no scope is open");
    let mut inside = scope.merk_cursor(subtree).expect("a cursor");
    inside.seek_to_last().expect("a seek");
    assert_eq!(inside.key(), Some(&key(89)[..]));
    let mut outside = tx.txn.merk_cursor(subtree).expect("a cursor");
    assert!(matches!(outside.node(|node| node.is_some()), Ok(false)));
    outside
        .seek_to_last()
        .expect("a scope that reads holds no reader off");
    assert_eq!(outside.key(), Some(&key(89)[..]));
    assert!(matches!(outside.node(|node| node.is_some()), Ok(true)));
    drop((inside, outside));
    scope.rollback();
    world.check(&tx);
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
}

/// A node the transaction wrote and took back: the cursor that stands on
/// its key reads no node there, and goes on from the key.
#[test]
fn a_merk_cursor_stands_on_a_key_whose_node_was_rolled_back() {
    let (db, versions) = Db::new(48);
    let mut world = World::new(&db, &versions);
    world.edit_outside(
        2,
        |items| {
            items.insert(b"a".to_vec(), b"1".to_vec());
            items.insert(b"c".to_vec(), b"3".to_vec());
        },
        &[],
    );
    let subtree = model::subtree(2);
    // With frames, and served by key after a commit outside met a write.
    for overlapped in [false, true] {
        let tx = world.begin();
        world.write(&tx, vec![put(aux(), b"mine", b"1")]);
        if overlapped {
            let outside = OwnedBatch {
                merks: Vec::new(),
                records: vec![put(aux(), b"mine", b"2")],
                ..OwnedBatch::default()
            };
            world.write_outside(&outside, &[&tx]);
        }
        let mark = tx.txn.mark();
        let writes = tx.model.borrow().writes.len();
        world.edit(&tx, 2, |items| {
            drop(items.insert(b"b".to_vec(), b"uncommitted".to_vec()))
        });
        let mut cursor = tx.txn.merk_cursor(subtree).expect("a cursor");
        cursor.seek(b"b").expect("a seek");
        assert_eq!(cursor.key(), Some(&b"b"[..]));
        assert!(cursor.node(|node| node.is_some()).expect("a read"));

        assert!(tx.txn.rewind(mark));
        tx.model.borrow_mut().writes.truncate(writes);
        assert_eq!(cursor.key(), Some(&b"b"[..]), "it stands on the key");
        assert!(cursor.node(|node| node.is_none()).expect("a read"));
        cursor.next().expect("a step");
        assert_eq!(cursor.key(), Some(&b"c"[..]));
        cursor.prev().expect("a step");
        assert_eq!(cursor.key(), Some(&b"a"[..]), "no node is between them");
        drop(cursor);
        world.check(&tx);
        assert_eq!(tx.txn.stats().overlapped > 0, overlapped);
    }
    world.check_committed();
}

/// T-RD7: a cursor of a transaction, with an empty write set and with a
/// non-empty one, keeps reading its opening version while commits outside
/// free the pages under it and make new ones.
#[test]
fn a_cursor_keeps_its_version_while_commits_free_and_reuse_pages() {
    let (db, versions) = Db::new(64);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(5);
    let unit = model::unit(Column::Data, 6);
    let key = |index: usize| format!("{index:05}").into_bytes();
    let batch = |records| OwnedBatch {
        merks: Vec::new(),
        records,
        ..OwnedBatch::default()
    };
    let fill = |rng: &mut Rng, from: usize| -> Vec<Write> {
        (from..from + 400)
            .map(|index| put(unit, &key(index), &value(rng, 40)))
            .collect()
    };
    world.write_outside(&batch(fill(&mut rng, 0)), &[]);

    for writes in [false, true] {
        let tx = world.begin();
        if writes {
            world.write(&tx, vec![put(unit, b"mine", b"1")]);
        }
        let opened = world.latest();
        let mut expected: Vec<Vec<u8>> = (tx.model.borrow().view(&world.model.records, unit))
            .into_keys()
            .collect();
        let mut cursor = tx.txn.cursor(unit);
        cursor.seek_to_first().expect("a seek");
        let freed = db
            .counts
            .pages_freed
            .load(std::sync::atomic::Ordering::Relaxed);
        // Commits outside empty the keyspace, fill it again and empty it,
        // while the cursor is half way.
        let mut seen = Vec::new();
        for round in 0..6 {
            for _ in 0..expected.len() / 8 {
                seen.extend(cursor.key().map(<[u8]>::to_vec));
                cursor.next().expect("a step");
            }
            let records = match round % 2 {
                0 => vec![Write::Range(unit, key(0), Some(key(90_000)))],
                _ => fill(&mut rng, 1_000 * (round + 1)),
            };
            world.write_outside(&batch(records), &[&tx]);
            assert_eq!(db.oldest_pin(&versions), opened, "the cursor's pin holds");
        }
        while let Some(key) = cursor.key() {
            seen.push(key.to_vec());
            cursor.next().expect("a step");
        }
        expected.sort();
        assert_eq!(seen, expected, "the cursor read its opening version");
        let freed_now = db
            .counts
            .pages_freed
            .load(std::sync::atomic::Ordering::Relaxed);
        assert!(freed_now > freed + 4, "pages under the cursor were freed");
        drop(cursor);
        // Without the cursor, the transaction reads the latest state.
        world.check(&tx);
        tx.txn.rollback().expect("no scope is open");
        world.write_outside(&batch(vec![Write::Range(unit, Vec::new(), None)]), &[]);
        world.write_outside(&batch(fill(&mut rng, 0)), &[]);
    }
    let reused = db
        .counts
        .ids_reused
        .load(std::sync::atomic::Ordering::Relaxed);
    assert!(reused > 4, "page ids were used again: {reused}");
    world.check_committed();
}

/// The write half of T-RD5.
#[test]
fn a_write_through_a_snapshot_is_refused() {
    let (db, versions) = Db::new(16);
    let snapshot = db.reads(&versions).snapshot();
    let records = [RecordWrite::put(Column::Aux, model::subtree(0), b"k", b"v")];
    let batch = Batch {
        records: &records,
        merks: &[],
        ..Batch::default()
    };
    assert_eq!(snapshot.apply(&batch), Err(SnapshotError::ReadOnly));
    assert_eq!(snapshot.commit(), Err(SnapshotError::ReadOnly));
    let world = World::new(&db, &versions);
    assert_eq!(get(&world.begin(), aux(), b"k"), None);
}
