//! Merks whose roots their hosts point at, and node values that are out of
//! line (`tree-layout.md` C3, S3, T3, `transactions.md` WT4, SC2).

use std::sync::atomic::Ordering;

use super::{
    model::{self, value, OwnedBatch, OwnedMerk},
    store::{Db, Outcome, Rng},
    *,
};

const PARENT: u8 = 10;

/// The shortest value that the store of these tests puts out of line.
const LONG: usize = lignin_tree::value::InlinePolicy::DEFAULT_THRESHOLD + 1;

/// The keys and values of one Merk.
type Items = BTreeMap<Vec<u8>, Vec<u8>>;

fn element(index: usize) -> Vec<u8> {
    format!("element-{index:03}").into_bytes()
}

fn child(index: usize) -> u8 {
    100 + index as u8
}

/// A batch that changes the children `changed` and, as every write
/// propagates to the root, the elements that host them, which name the
/// children's roots again.
fn hosted_batch<'s>(
    world: &World<'s>,
    tx: &Tx<'s>,
    changed: &[usize],
    edit: &mut dyn FnMut(usize, &mut Items),
    more: &mut dyn FnMut(&mut Items),
) -> OwnedBatch {
    let mut merks: Vec<OwnedMerk> = Vec::new();
    for index in changed {
        merks.push(world.merk(tx, child(*index), |items| edit(*index, items)));
    }
    let roots: Vec<Option<Vec<u8>>> = (changed.iter().zip(&merks))
        .map(|(index, merk)| {
            let unit = Unit::data(model::subtree(child(*index)));
            let view = tx.model.borrow().view(&world.model.records, unit);
            match &merk.root {
                Some(root) => root.clone(),
                None => model::records(&model::Committed::items(&view)).0,
            }
        })
        .collect();
    let mut parent = world.merk(tx, PARENT, |items| {
        for (index, root) in changed.iter().zip(&roots) {
            // The element holds the key of its Merk's root, as today.
            let mut value = b"tree:".to_vec();
            value.extend_from_slice(root.as_deref().unwrap_or(b"-"));
            value.extend_from_slice(&[merks.len() as u8; 30]);
            items.insert(element(*index), value);
        }
        more(items);
    });
    for index in changed {
        (parent.hosted).insert(element(*index), vec![model::subtree(child(*index))]);
    }
    let records = World::root_record(&parent).into_iter().collect();
    merks.push(parent);
    OwnedBatch {
        merks,
        records,
        ..OwnedBatch::default()
    }
}

#[test]
fn a_host_points_at_the_root_of_the_merk_it_hosts() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(51);
    let hosted = || db.counts.hosted.load(Ordering::Relaxed);
    let all: Vec<usize> = (0..30).collect();

    // Thirty elements, each with a small Merk under it, in one operation.
    let tx = world.begin();
    let batch = hosted_batch(
        &world,
        &tx,
        &all,
        &mut |index, items| {
            for node in 0..1 + index % 3 {
                items.insert(vec![b'n', node as u8], value(&mut rng, 40));
            }
        },
        &mut |_| {},
    );
    world.apply(&tx, &batch).expect("the Merks are made");
    world.check(&tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
    assert_eq!(hosted(), 30, "every host points at its Merk's root");

    // The parent grows, and its pages are cut: hosts are carried, and the
    // small Merks under them go along. Their directory entries follow.
    let tx = world.begin();
    let roots = db.roots(&versions, versions.latest());
    for round in 0..6 {
        let batch = hosted_batch(&world, &tx, &[], &mut |_, _| {}, &mut |items| {
            for index in 0..40 {
                let key = format!("element-{:03}-{round}-{index}", rng.below(30));
                items.insert(key.into_bytes(), value(&mut rng, 150));
            }
        });
        world.apply(&tx, &batch).expect("the parent grows");
        world.check(&tx);
    }
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
    // No batch named a child, and the directory has other roots for some:
    // what the write's report said of the cells it carried.
    let now = db.roots(&versions, versions.latest());
    let carried = (roots.iter()).filter(|(subtree, root)| {
        **subtree != model::subtree(PARENT) && now.get(*subtree) != Some(*root)
    });
    assert!(carried.count() > 0, "roots went along with their hosts");
    assert_eq!(hosted(), 30);

    // Children get other roots, become empty and are filled again, each
    // with its host written in the same operation.
    let tx = world.begin();
    for round in 0..5 {
        let changed: Vec<usize> = (0..30).filter(|index| (index + round) % 4 == 0).collect();
        let batch = hosted_batch(
            &world,
            &tx,
            &changed,
            &mut |index, items| {
                if (index + round) % 8 == 0 {
                    items.clear();
                } else {
                    for node in 0..12 {
                        items.insert(vec![b'm', round as u8, node], value(&mut rng, 90));
                    }
                }
            },
            &mut |_| {},
        );
        world.apply(&tx, &batch).expect("the children change");
        world.check(&tx);
    }
    // A commit outside to a Merk of its own, and the transaction is rebased.
    world.edit_outside(
        1,
        |items| drop(items.insert(b"k".to_vec(), b"v".to_vec())),
        &[&tx],
    );
    world.check(&tx);
    assert_eq!(tx.txn.stats().rebuilds, 1);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
    // A Merk with no node has no root for its host to point at.
    let with_nodes = |world: &World<'_>| {
        let has = |index: &usize| {
            let unit = Unit::data(model::subtree(child(*index)));
            world.model.records.keys().any(|(of, _)| *of == unit)
        };
        (0..30).filter(has).count() as u64
    };
    let filled = with_nodes(&world);
    assert!((10..30).contains(&filled), "{filled} children have nodes");
    assert_eq!(hosted(), filled);

    // A write that stops below the host (iteration two): the root stays in
    // its page, and the directory entry is exact.
    let tx = world.begin();
    let mut merk = world.merk(&tx, child(1), |items| {
        let (key, _) = items.iter().next().expect("the Merk has nodes");
        items.insert(key.clone(), b"changed below the host".to_vec());
    });
    assert!(merk.root.is_none(), "the root is the node it was");
    merk.anchored = true;
    let merks = vec![merk];
    let records = Vec::new();
    world
        .apply(
            &tx,
            &OwnedBatch {
                merks,
                records,
                ..OwnedBatch::default()
            },
        )
        .expect("a write below the host");
    world.check(&tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
    assert_eq!(hosted(), filled);
}

/// A batch that changes a Merk and writes its host says in the host which
/// Merk that is. A host that keeps its pointers names nothing, and where
/// its cell shows that it hosts the Merk, the batch is refused.
#[test]
fn a_host_that_keeps_its_pointers_beside_its_merk_is_refused() {
    let (db, versions) = Db::new(64);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(53);
    let tx = world.begin();
    let batch = hosted_batch(
        &world,
        &tx,
        &[0, 1, 2],
        &mut |_, items| drop(items.insert(b"n".to_vec(), value(&mut rng, 40))),
        &mut |_| {},
    );
    world.apply(&tx, &batch).expect("the Merks are made");
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();

    // As the transaction's first write, and after another.
    for written in [false, true] {
        let tx = world.begin();
        if written {
            world.write(&tx, vec![put(model::unit(Column::Aux, 0), b"k", b"1")]);
        }
        // The element changes with its Merk, as it does where it holds the
        // Merk's root hash.
        let element_now = value(&mut rng, 45);
        let mut batch = hosted_batch(
            &world,
            &tx,
            &[1],
            &mut |_, items| drop(items.insert(b"m".to_vec(), value(&mut rng, 40))),
            &mut |items| drop(items.insert(element(1), element_now.clone())),
        );
        let parent = batch.merks.last_mut().expect("the parent is last");
        assert!(parent.nodes.iter().any(|(key, ..)| *key == element(1)));
        parent.hosted.clear();
        let error = world
            .apply(&tx, &batch)
            .expect_err("the host names nothing");
        assert!(
            matches!(
                error,
                TxnError::Batch(BatchError::HostKeeps { subtree }) if subtree == model::subtree(child(1))
            ),
            "{error}"
        );
        world.check(&tx);
        // With the Merk named in its host, the same change goes through.
        let batch = hosted_batch(
            &world,
            &tx,
            &[1],
            &mut |_, items| drop(items.insert(b"m".to_vec(), value(&mut rng, 40))),
            &mut |items| drop(items.insert(element(1), element_now.clone())),
        );
        world.apply(&tx, &batch).expect("the host names its Merk");
        world.check(&tx);
        assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
        world.check_committed();
    }
    assert_eq!(db.private_ids(), 0);
}

#[test]
fn a_value_that_is_out_of_line_stays_with_the_transaction() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(52);
    fn key(index: usize) -> Vec<u8> {
        format!("v{index:03}").into_bytes()
    }
    /// A batch that gives the Merk `count` nodes from `from` on, two of
    /// three with a value that is longer than the store keeps in a cell.
    fn long<'s>(
        world: &World<'s>,
        tx: &Tx<'s>,
        rng: &mut Rng,
        from: usize,
        count: usize,
    ) -> OwnedBatch {
        let merk = world.merk(tx, 1, |items| {
            for index in from..from + count {
                let len = match index % 3 {
                    0 => 20,
                    _ => LONG + rng.below(1_500),
                };
                items.insert(key(index), value(rng, len));
            }
        });
        let records = World::root_record(&merk).into_iter().collect();
        let merks = vec![merk];
        OwnedBatch {
            merks,
            records,
            ..OwnedBatch::default()
        }
    }
    let subtree = model::subtree(1);
    let pages_with_values = |tx: &Tx<'_>| tx.txn.stats().value_pages;

    // The length alone says where a value goes: one of the threshold's
    // length is in its cell, and one byte more is out of line.
    let tx = world.begin();
    assert_eq!(tx.txn.path().inline_policy().threshold(), LONG - 1);
    world.edit(&tx, 2, |items| {
        items.insert(b"at".to_vec(), vec![1; LONG - 1]);
    });
    assert_eq!(pages_with_values(&tx), 0);
    world.edit(&tx, 2, |items| {
        items.insert(b"over".to_vec(), vec![2; LONG]);
        items.insert(b"none".to_vec(), Vec::new());
    });
    assert_eq!(pages_with_values(&tx), 1);
    world.check(&tx);
    tx.txn.rollback().expect("no scope is open");
    tx.forget();

    let batch = long(&world, &tx, &mut rng, 0, 40);
    world.apply(&tx, &batch).expect("values out of line");
    // The transaction reads the bytes it holds (WT4).
    world.check(&tx);
    let first = pages_with_values(&tx);
    assert!(first > 0);
    // More nodes cut the pages: cells are carried with their values.
    let mark = tx.txn.mark();
    let writes = tx.model.borrow().writes.len();
    for round in 1..4 {
        let batch = long(&world, &tx, &mut rng, round * 40, 40);
        world.apply(&tx, &batch).expect("values out of line");
        world.check(&tx);
    }
    assert!(pages_with_values(&tx) > first);
    // A rollback to the mark gives the later values up.
    tx.txn.rewind(mark);
    tx.model.borrow_mut().writes.truncate(writes);
    world.check(&tx);
    assert_eq!(pages_with_values(&tx), first);
    // A rebase holds them again, from the write set.
    world.edit_outside(
        2,
        |items| drop(items.insert(b"k".to_vec(), b"v".to_vec())),
        &[&tx],
    );
    world.check(&tx);
    assert_eq!(tx.txn.stats().rebuilds, 2);
    assert!(pages_with_values(&tx) > 0);
    // A value is replaced by one in its cell, and a node is deleted.
    world.edit(&tx, 1, |items| {
        items.insert(key(1), b"short now".to_vec());
        items.remove(&key(2));
    });
    world.check(&tx);
    // Overlapped, it serves the bytes from its write set.
    let outside = world.begin();
    world.write(
        &outside,
        vec![put(Unit::new(Column::Roots, subtree), b"root", b"x")],
    );
    assert!(matches!(
        world.commit(outside, &[&tx]),
        Outcome::Committed(_)
    ));
    world.check(&tx);
    assert!(tx.txn.stats().overlapped > 0);
    assert_eq!(pages_with_values(&tx), 0);
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);

    // Committed, such a value is with the frame of its page until
    // write-back places it (FL7), and whoever reads the node reads its
    // bytes there.
    let attached = || db.counts.value_pages.load(Ordering::Relaxed);
    let before = attached();
    let tx = world.begin();
    let batch = long(&world, &tx, &mut rng, 0, 9);
    world.apply(&tx, &batch).expect("values out of line");
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    assert!(attached() > before);
    world.check_committed();
    // A later transaction cuts the pages: cells are carried with values
    // that the earlier commit left pending, and its commit repeats them
    // for their new pages.
    let tx = world.begin();
    let batch = long(&world, &tx, &mut rng, 9, 60);
    world.apply(&tx, &batch).expect("values out of line");
    world.check(&tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();

    // A transaction rewrites nodes around such values, which it keeps:
    // its frames read them where they are.
    let tx = world.begin();
    world.edit(&tx, 1, |items| {
        drop(items.insert(key(0), b"changed".to_vec()))
    });
    world.check(&tx);
    // A commit outside then overwrites those nodes. The transaction takes
    // its own copy of every value it kept, and goes on reading them.
    let outside = world.begin();
    world.edit(&outside, 1, |items| items.clear());
    assert!(matches!(
        world.commit(outside, &[&tx]),
        Outcome::Committed(_)
    ));
    world.check(&tx);
    assert!(tx.txn.stats().overlapped > 0);
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
}
