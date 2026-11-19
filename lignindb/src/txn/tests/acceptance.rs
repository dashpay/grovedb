//! The three sentences of the task's acceptance test, each on a store on
//! disk: uncommitted changes are invisible to every other handle, a cursor
//! inside the transaction sees them in key order, and a read inside the
//! transaction sees a write committed outside it after it started, unless
//! the transaction wrote that key itself.

use super::{
    model::{self, value, OwnedBatch},
    store::{Db, Outcome, Rng},
    *,
};

fn key(index: usize) -> Vec<u8> {
    format!("key-{index:04}").into_bytes()
}

/// A store with records in two flat units and in two Merks.
fn filled(world: &mut World<'_>, rng: &mut Rng) {
    let aux = model::unit(Column::Aux, 0);
    let meta = model::unit(Column::Meta, 9);
    let mut records = Vec::new();
    for index in 0..60 {
        let len = 1 + rng.below(90);
        records.push(put(aux, &key(index * 2), &value(rng, len)));
    }
    records.push(put(meta, b"m", b"meta"));
    let merks = Vec::new();
    let outcome = world.write_outside(&OwnedBatch { merks, records }, &[]);
    assert!(matches!(outcome, Outcome::Committed(_)));
    for name in [1, 2] {
        world.edit_outside(
            name,
            |items| {
                for index in 0..70 {
                    let len = 20 + rng.below(160);
                    items.insert(key(index * 2), value(rng, len));
                }
            },
            &[],
        );
    }
    world.check_committed();
}

#[test]
fn uncommitted_changes_are_invisible_to_every_other_handle() {
    let (db, versions) = Db::new(64);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(1);
    filled(&mut world, &mut rng);
    let aux = model::unit(Column::Aux, 0);
    let before = world.latest();
    let snapshot = db.reads(&versions).snapshot();

    let tx = world.begin();
    world.write(
        &tx,
        vec![
            put(aux, b"mine", b"uncommitted"),
            put(aux, &key(4), b"replaced"),
            delete(aux, &key(6)),
            Write::Range(aux, key(20), Some(key(40))),
        ],
    );
    world.edit(&tx, 1, |items| {
        for index in 0..40 {
            items.insert(key(index * 2 + 1), value(&mut rng, 100));
        }
        for index in 10..30 {
            items.remove(&key(index * 2));
        }
        items.insert(key(2), b"another value".to_vec());
    });
    world.edit(&tx, 3, |items| {
        items.insert(b"new".to_vec(), b"in a Merk the transaction made".to_vec());
    });
    // The transaction reads its own writes.
    world.check(&tx);
    let stats = tx.txn.stats();
    assert!(stats.tree_pages > 0 && stats.flat_pages > 0, "{stats:?}");
    assert_eq!((stats.base, stats.pinned), (Some(before), Some(before)));
    assert!(db.private_ids() > 0, "the transaction made pages");

    // Nobody else does: a transaction that has written nothing, a second
    // one with writes of its own, a snapshot, and whoever reads the pages
    // of the latest version.
    world.check_committed();
    let other = world.begin();
    world.write(&other, vec![put(aux, b"other", b"also uncommitted")]);
    world.check(&other);
    world.check(&tx);
    assert_eq!(world.latest(), before, "nothing was committed");
    assert_eq!(snapshot.version(), before);
    let keys = db.check_trees(&versions, snapshot.version());
    assert_eq!(keys[&model::subtree(1)].len(), 70);
    assert!(!keys.contains_key(&model::subtree(3)));

    // A rollback leaves nothing behind (WT9).
    tx.txn.rollback().expect("no scope is open");
    tx.forget();
    world.check(&tx);
    assert_eq!(tx.txn.stats().writes, 0);
    other.txn.rollback().expect("no scope is open");
    assert_eq!(db.private_ids(), 0, "the ids went back");
    assert_eq!(db.pool.census().private, 0, "the copies went");
    world.check_committed();

    // The same writes, committed, are what everybody reads.
    world.write(
        &tx,
        vec![put(aux, b"mine", b"committed"), delete(aux, &key(6))],
    );
    world.edit(&tx, 1, |items| {
        items.insert(key(1), b"now it counts".to_vec());
        items.remove(&key(8));
    });
    world.check(&tx);
    let outcome = world.commit(tx, &[]);
    assert_eq!(outcome, Outcome::Committed(world.latest()));
    assert!(world.latest() > before);
    world.check_committed();
    // The snapshot from before still reads its version.
    assert_eq!(
        db.check_trees(&versions, snapshot.version())[&model::subtree(1)].len(),
        70
    );
    assert_eq!(
        db.check_trees(&versions, world.latest())[&model::subtree(1)].len(),
        70
    );
}

#[test]
fn a_cursor_inside_the_transaction_sees_its_changes_in_key_order() {
    let (db, versions) = Db::new(64);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(2);
    filled(&mut world, &mut rng);
    let aux = model::unit(Column::Aux, 0);
    let committed = scan(&mut world.begin().txn.cursor(aux), false);
    assert_eq!(committed.len(), 60);

    // New keys before, between and after the committed ones, a new value
    // for a committed key, a delete and a range delete.
    let tx = world.begin();
    let puts: [(Vec<u8>, &[u8]); 4] = [
        (b"a-first".to_vec(), b"before every committed key"),
        (key(3), b"between two committed keys"),
        (key(10), b"in place of a committed value"),
        (b"zz-last".to_vec(), b"after every committed key"),
    ];
    let mut writes: Vec<Write> = (puts.iter())
        .map(|(key, value)| put(aux, key, value))
        .collect();
    writes.push(delete(aux, &key(12)));
    writes.push(Write::Range(aux, key(40), Some(key(60))));
    world.write(&tx, writes);

    let mut expected: BTreeMap<Vec<u8>, Vec<u8>> = committed.iter().cloned().collect();
    for (key, value) in &puts {
        expected.insert(key.clone(), value.to_vec());
    }
    expected.remove(&key(12));
    expected.retain(|at, _| !(key(40)..key(60)).contains(at));
    let expected: Vec<(Vec<u8>, Vec<u8>)> = expected.into_iter().collect();
    assert_eq!(expected.len(), 60 + 3 - 1 - 10);

    let forward = scan(&mut tx.txn.cursor(aux), false);
    assert!(forward.windows(2).all(|pair| pair[0].0 < pair[1].0));
    assert_eq!(forward, expected, "every change, in key order");
    let mut back = scan(&mut tx.txn.cursor(aux), true);
    back.reverse();
    assert_eq!(back, expected, "and in the other direction");
    // A step goes from a committed key to the transaction's own next one,
    // and on to the next committed one.
    let mut cursor = tx.txn.cursor(aux);
    cursor.seek(&key(2)).expect("a seek");
    for at in [key(2), key(3), key(4)] {
        assert_eq!(cursor.key(), Some(&at[..]));
        cursor.next().expect("a step");
    }
    drop(cursor);

    // The cursor of another transaction sees none of it.
    assert_eq!(scan(&mut world.begin().txn.cursor(aux), false), committed);
    world.check(&tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    assert_eq!(scan(&mut world.begin().txn.cursor(aux), false), expected);
    world.check_committed();
}

#[test]
fn a_read_inside_the_transaction_sees_a_write_committed_outside_unless_it_wrote_the_key() {
    let (db, versions) = Db::new(64);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(3);
    filled(&mut world, &mut rng);
    let aux = model::unit(Column::Aux, 0);
    fn get(tx: &Tx<'_>, unit: Unit, key: &[u8]) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        let found = tx.txn.get(unit, key, &mut out).expect("a read");
        found.then_some(out)
    }
    fn node(tx: &Tx<'_>, name: u8, key: &[u8]) -> Option<Vec<u8>> {
        let read = |view: Option<NodeView<'_>>| {
            view.map(|view| match view.value() {
                NodeValue::Bytes(bytes) => bytes.to_vec(),
                other => panic!("the value is {other:?}"),
            })
        };
        (tx.txn.node(model::subtree(name), key, read)).expect("a read")
    }

    // The transaction starts, and writes one flat key and one node.
    let tx = world.begin();
    let started = world.latest();
    world.write(&tx, vec![put(aux, &key(2), b"the transaction's")]);
    world.edit(&tx, 1, |items| {
        drop(items.insert(key(2), b"the transaction's node".to_vec()))
    });

    // Afterwards, commits outside it write those two keys and others: a
    // flat key that was there, one that was not, and nodes of both Merks.
    let outside = OwnedBatch {
        merks: Vec::new(),
        records: vec![
            put(aux, &key(2), b"outside"),
            put(aux, &key(4), b"outside"),
            put(aux, b"new", b"outside"),
        ],
    };
    assert!(matches!(
        world.write_outside(&outside, &[&tx]),
        Outcome::Committed(_)
    ));
    for name in [1, 2] {
        world.edit_outside(
            name,
            |items| {
                items.insert(key(2), b"outside node".to_vec());
                items.insert(b"new".to_vec(), b"outside node".to_vec());
            },
            &[&tx],
        );
    }
    assert!(world.latest() > started);

    // What it has not written, it reads as committed outside.
    assert_eq!(get(&tx, aux, &key(4)), Some(b"outside".to_vec()));
    assert_eq!(get(&tx, aux, b"new"), Some(b"outside".to_vec()));
    assert_eq!(node(&tx, 2, &key(2)), Some(b"outside node".to_vec()));
    assert_eq!(node(&tx, 2, b"new"), Some(b"outside node".to_vec()));
    assert_eq!(node(&tx, 1, b"new"), Some(b"outside node".to_vec()));
    // What it wrote itself, it reads as it wrote it.
    assert_eq!(get(&tx, aux, &key(2)), Some(b"the transaction's".to_vec()));
    assert_eq!(
        node(&tx, 1, &key(2)),
        Some(b"the transaction's node".to_vec())
    );
    // Every other record too, against the model.
    world.check(&tx);
    // Its keys were written outside after it wrote them: its commit fails,
    // and the commits outside stand.
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);
    let reader = world.begin();
    assert_eq!(get(&reader, aux, &key(2)), Some(b"outside".to_vec()));
    assert_eq!(node(&reader, 1, &key(2)), Some(b"outside node".to_vec()));
    world.check_committed();
}
