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
