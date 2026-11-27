//! How often a batch writes a leaf of the subtree directory: once for the
//! records it puts there, and not once a record (`lignin_tree::directory`,
//! "The records of a batch"). The frames count what each batch settles
//! (`frames::probe`), and the tests hold the counts to the committed
//! directory before the batch and after it, and to every image of a leaf
//! that the thread wrote into a flat keyspace while the batch was applied
//! (`lignin_tree::flat::leaf_images_written`).

use std::collections::{BTreeMap, BTreeSet};

use lignin_format::{flat_page::FlatPage, CommitVersion, PageId};
use lignin_store::load::FRAME_FORMAT;
use lignin_tree::{
    directory::{DirectoryError, Settled},
    flat::{leaf_images_written, FlatError, FlatPages},
};

use super::{
    model::{self, handle_of, OwnedBatch, OwnedSubtree},
    store::{Db, Outcome, ENTERED},
    subtrees::{enter, hosted, subtrees},
    *,
};
use crate::{snapshot::Versions, txn::frames::probe};

/// A Merk that every store of the tests has entered, in which these tests
/// put the nodes that host subtrees.
const HOST: u8 = 7;

fn sub(name: u8) -> Subtree {
    model::subtree(name)
}

fn committed(outcome: Outcome) -> bool {
    matches!(outcome, Outcome::Committed(_))
}

fn key_of(name: u8) -> Vec<u8> {
    vec![b'h', name]
}

/// The directory of the committed version `at`: every record, and every
/// leaf of its keyspace with the first key beyond its key range, in the
/// order of the keys.
struct Seen {
    records: BTreeMap<Vec<u8>, Vec<u8>>,
    leaves: Vec<(PageId, Option<Vec<u8>>)>,
}

impl Seen {
    fn at(db: &Db, versions: &Versions, at: CommitVersion) -> Self {
        let guard = versions.guard();
        let pages = db.committed(&guard, at);
        let tree = db.directory();
        let mut records = BTreeMap::new();
        let mut cursor = tree.cursor();
        cursor.seek_to_first(&pages).expect("a walk");
        while let Some(key) = cursor.key() {
            let mut value = Vec::new();
            assert!(cursor.value(&pages, &mut value).expect("a value"));
            records.insert(key.to_vec(), value);
            cursor.next(&pages).expect("a walk");
        }
        let mut leaves = Vec::new();
        let mut left = vec![(tree.root(), None)];
        while let Some((page, end)) = left.pop() {
            let frame = pages.page(page).expect("a page");
            match FlatPage::from_validated(&frame, FRAME_FORMAT).expect("a flat page") {
                FlatPage::Leaf(_) => leaves.push((page, end)),
                FlatPage::Branch(branch) => {
                    // The last child first, so that the first is taken
                    // next.
                    for index in (0..=branch.len()).rev() {
                        let child = branch.child(index).expect("a child");
                        let until = branch.separator(index).map(|key| key.to_vec());
                        left.push((child, until.or_else(|| end.clone())));
                    }
                }
            }
        }
        Self { records, leaves }
    }

    /// The leaf whose key range holds `key`.
    fn leaf_of(&self, key: &[u8]) -> PageId {
        let within = |end: &Option<Vec<u8>>| end.as_deref().is_none_or(|end| key < end);
        let found = self.leaves.iter().find(|(_, end)| within(end));
        found.expect("the last leaf has no end").0
    }

    fn pages(&self) -> BTreeSet<PageId> {
        self.leaves.iter().map(|(page, _)| *page).collect()
    }
}

/// Applies the batch that `make` gives in a transaction of its own and
/// commits it, and returns what the batch settled in the directory, held
/// to the committed directory before and after:
///
/// - the records that were settled are the records that differ;
/// - the leaves that took them are the leaves those keys lay in before;
/// - where the keyspace has the leaves it had, each of those leaves was
///   written once and no other was written;
/// - where it has those and more, each of those leaves was written once,
///   and one image more for each leaf that a cut made;
/// - **those are all the images of leaves that were written while the
///   batch was applied**, into the directory or into any other flat
///   keyspace: a batch of these tests writes no flat record, so a write
///   of the directory that the frames' count did not see would show here.
///
/// Returns with it how many leaves the keyspace gained.
fn settled_by<'s>(
    world: &mut World<'s>,
    make: impl FnOnce(&World<'s>, &Tx<'s>) -> OwnedBatch,
) -> (Settled, usize) {
    let (db, versions) = (world.db, world.versions);
    let pin = versions.pin();
    let before = Seen::at(db, versions, pin.version());
    let tx = world.begin();
    let batch = make(world, &tx);
    probe::take_settled();
    let images = leaf_images_written();
    world.apply(&tx, &batch).expect("the batch is applied");
    let images = leaf_images_written() - images;
    let settled = probe::take_settled();
    assert_eq!(
        images, settled.leaf_writes,
        "every image of a leaf that was written is one that the batch settled"
    );
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    assert_eq!(probe::take_settled(), Settled::default());
    world.check_committed();
    let after = Seen::at(db, versions, world.latest());

    let keys: BTreeSet<&Vec<u8>> = before.records.keys().chain(after.records.keys()).collect();
    let differs = |key: &&&Vec<u8>| before.records.get(**key) != after.records.get(**key);
    let changed: Vec<&Vec<u8>> = keys.iter().filter(differs).copied().collect();
    let homes: BTreeSet<PageId> = changed.iter().map(|key| before.leaf_of(key)).collect();
    assert_eq!(
        settled.records,
        changed.len() as u64,
        "the records that differ"
    );
    assert_eq!(settled.leaves, homes.len() as u64, "the leaves they lay in");
    let (had, has) = (before.pages(), after.pages());
    assert!(had.is_subset(&has), "no leaf left the keyspace");
    let cut = has.len() - had.len();
    assert_eq!(
        settled.leaf_writes,
        settled.leaves + cut as u64,
        "each leaf once, and the pieces of those that were cut"
    );
    drop(pin);
    (settled, cut)
}

/// A store whose Merk [`HOST`] has a node for each of `names` that hosts
/// the subtree of that name, each with `nodes` nodes of its own.
fn grove<'s>(world: &mut World<'s>, names: &[u8], nodes: u8) {
    let tx = world.begin();
    let mut host = world.merk(&tx, HOST, |items| {
        for name in names {
            items.insert(key_of(*name), b"a tree".to_vec());
        }
    });
    let mut merks = Vec::new();
    for name in names {
        host.hosted.insert(key_of(*name), vec![sub(*name)]);
        if nodes > 0 {
            merks.push(world.merk(&tx, *name, |items| {
                for node in 0..nodes {
                    items.insert(vec![b'n', node], vec![*name; 20]);
                }
            }));
        }
    }
    merks.push(host);
    let batch = OwnedBatch {
        merks,
        subtrees: (names.iter())
            .map(|name| hosted(*name, HOST, &key_of(*name)))
            .collect(),
        ..OwnedBatch::default()
    };
    world.apply(&tx, &batch).expect("the subtrees are made");
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

fn names(count: u8) -> Vec<u8> {
    (0..count).map(|n| ENTERED + n).collect()
}

/// Enters: of each Merk that stands alone its entry, and the header once.
/// Of each hosted one the record of its host as well, and the root that
/// the Merk of the hosts gets, in the cell address's place and in its
/// entry.
#[test]
fn a_batch_of_enters_writes_each_leaf_of_the_directory_once() {
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let alone = names(24);
    let (settled, same) = settled_by(&mut world, |_, _| {
        subtrees(alone.iter().map(|name| enter(*name)).collect())
    });
    assert_eq!(same, 0, "no leaf is cut");
    assert_eq!(settled.records, 24 + 1);
    println!("24 enters of Merks that stand alone: {settled:?}");
    assert!(settled.leaves < 8, "{settled:?}");

    let below: Vec<u8> = (30..50).map(|n| ENTERED + n).collect();
    let (settled, cut) = settled_by(&mut world, |world, tx| {
        let mut host = world.merk(tx, HOST, |items| {
            for name in &below {
                items.insert(key_of(*name), b"a tree".to_vec());
            }
        });
        for name in &below {
            host.hosted.insert(key_of(*name), vec![sub(*name)]);
        }
        OwnedBatch {
            merks: vec![host],
            subtrees: (below.iter())
                .map(|name| hosted(*name, HOST, &key_of(*name)))
                .collect(),
            ..OwnedBatch::default()
        }
    });
    // An entry and the record of its host for each, the header, and the
    // first root of the Merk that hosts them: its entry and the record of
    // the root.
    assert_eq!(settled.records, 2 * 20 + 1 + 2);
    println!("20 enters of hosted Merks: {settled:?}, with {cut} leaves cut");
    assert!(cut == 1 && settled.leaves < 10, "{settled:?}");
}

/// Drops: of each Merk its entry and the record of its host go, and the
/// record of its root where it has a node, and a record of what it left is
/// put.
#[test]
fn a_batch_of_drops_writes_each_leaf_of_the_directory_once() {
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let all = names(30);
    grove(&mut world, &all, 2);
    let gone: Vec<u8> = all.iter().copied().step_by(2).collect();
    let (settled, same) = settled_by(&mut world, |world, tx| {
        let host = world.merk(tx, HOST, |items| {
            for name in &gone {
                items.remove(&key_of(*name));
            }
        });
        OwnedBatch {
            merks: vec![host],
            subtrees: (gone.iter())
                .map(|name| OwnedSubtree::Drop(sub(*name)))
                .collect(),
            ..OwnedBatch::default()
        }
    });
    println!("15 drops of hosted Merks with nodes: {settled:?}");
    // Four records of each; the Merk that hosts them may have another
    // root, or its root another cell.
    assert!(
        (4 * 15..=4 * 15 + 3).contains(&settled.records),
        "{settled:?}"
    );
    assert!(same == 0 && settled.leaves < 10, "{settled:?}");
}

/// Clears: of each Merk with a node the record of its root goes, its
/// entry and the record of its host are written with its new id, and a
/// record of what it left is put. The header is written once.
#[test]
fn a_batch_of_clears_writes_each_leaf_of_the_directory_once() {
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let all = names(30);
    grove(&mut world, &all, 3);
    let cleared: Vec<u8> = all.iter().copied().step_by(2).collect();
    let (settled, same) = settled_by(&mut world, |world, tx| {
        let mut host = world.merk(tx, HOST, |items| {
            for name in &cleared {
                items.insert(key_of(*name), b"a tree with no node".to_vec());
            }
        });
        for name in &cleared {
            host.hosted.insert(key_of(*name), vec![sub(*name)]);
        }
        OwnedBatch {
            merks: vec![host],
            subtrees: (cleared.iter())
                .map(|name| OwnedSubtree::Clear(sub(*name)))
                .collect(),
            ..OwnedBatch::default()
        }
    });
    println!("15 clears of hosted Merks with nodes: {settled:?}");
    assert!(
        (4 * 15 + 1..=4 * 15 + 4).contains(&settled.records),
        "{settled:?}"
    );
    assert!(same == 0 && settled.leaves < 10, "{settled:?}");
}

/// Moves: of each Merk the entry and the record of its host go and are
/// written under the new prefix and the new key, and its root's record
/// names the new prefix.
#[test]
fn a_batch_of_moves_writes_each_leaf_of_the_directory_once() {
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let all = names(16);
    grove(&mut world, &all, 2);
    let to = |name: u8| name + 20;
    let (settled, same) = settled_by(&mut world, |world, tx| {
        let reads: Vec<_> = (all.iter())
            .map(|name| (tx.txn.node(sub(HOST), &key_of(*name), handle_of)).expect("a read"))
            .collect();
        let mut host = world.merk(tx, HOST, |items| {
            for name in &all {
                let value = items.remove(&key_of(*name)).expect("the node of the host");
                items.insert(key_of(to(*name)), value);
            }
        });
        for name in &all {
            host.hosted.insert(key_of(to(*name)), vec![sub(to(*name))]);
        }
        let moves = all
            .iter()
            .zip(reads)
            .map(|(name, read)| OwnedSubtree::Move {
                from: sub(*name),
                to: sub(to(*name)),
                host: (sub(HOST), key_of(to(*name)), 0),
                read,
                below: Vec::new(),
            });
        OwnedBatch {
            merks: vec![host],
            subtrees: moves.collect(),
            ..OwnedBatch::default()
        }
    });
    println!("16 moves of hosted Merks with nodes: {settled:?}");
    // Five records of each; the Merk that hosts them may have another
    // root, or its root another cell.
    assert!(
        (5 * 16..=5 * 16 + 3).contains(&settled.records),
        "{settled:?}"
    );
    assert!(same == 0 && settled.leaves < 10, "{settled:?}");
}

/// A batch that is applied again, where the frames are made again after a
/// rollback to a savepoint, settles its records again: the leaves are as
/// if the records of the batches that were rolled back had never been
/// put. So is the directory after a write scope that is taken out.
#[test]
fn a_rollback_leaves_the_directory_as_if_the_records_were_never_put() {
    let made = |rolled_back: bool| {
        let (db, versions) = Db::new(256);
        let mut world = World::new(&db, &versions);
        grove(&mut world, &names(12), 2);
        let tx = world.begin();
        let kept = subtrees((40..46).map(|n| enter(ENTERED + n)).collect());
        world.apply(&tx, &kept).expect("entered");
        if rolled_back {
            tx.txn.set_savepoint().expect("no scope is open");
            let writes = tx.model.borrow().writes.len();
            let host = world.merk(&tx, HOST, |items| {
                for name in names(6) {
                    items.remove(&key_of(name));
                }
            });
            let batch = OwnedBatch {
                merks: vec![host],
                subtrees: (names(6).into_iter())
                    .map(|name| OwnedSubtree::Drop(sub(name)))
                    .chain((46..52).map(|n| enter(ENTERED + n)))
                    .collect(),
                ..OwnedBatch::default()
            };
            world.apply(&tx, &batch).expect("dropped and entered");
            world.check(&tx);
            tx.txn.rollback_to_savepoint().expect("a savepoint is set");
            tx.model.borrow_mut().writes.truncate(writes);
            world.check(&tx);

            // And a scope that is taken out.
            let scope = tx.txn.scope().expect("a scope");
            let entered = subtrees((46..52).map(|n| enter(ENTERED + n)).collect());
            entered
                .with(|batch| scope.apply(batch))
                .expect("entered in the scope");
            scope.rollback();
            world.check(&tx);
        }
        assert!(committed(world.commit(tx, &[])));
        world.check_committed();
        let seen = Seen::at(&db, &versions, world.latest());
        let guard = versions.guard();
        let pages = db.committed(&guard, world.latest());
        let images: Vec<Vec<u8>> = (seen.leaves.iter())
            .map(|(page, _)| {
                let frame = pages.page(*page).expect("a page");
                let read = FlatPage::from_validated(&frame, FRAME_FORMAT).expect("a flat page");
                read.as_bytes().to_vec()
            })
            .collect();
        (seen.records, seen.leaves, images)
    };
    let (plain, rolled) = (made(false), made(true));
    assert!(plain.0 == rolled.0, "the records of the directory");
    assert_eq!(plain.1, rolled.1, "its leaves and their key ranges");
    assert!(plain.2 == rolled.2, "the images of its leaves");
}

/// A batch whose settling fails after it wrote a leaf of the directory
/// leaves nothing of itself: the frames are made again from the writes
/// that were taken before it, the batch goes through when it is applied
/// again, and the transaction commits.
#[test]
fn a_settling_that_fails_after_a_leaf_leaves_nothing_of_its_batch() {
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let tx = world.begin();
    // A write that is taken before: Merks, and a record beside them.
    let before = subtrees((30..36).map(|n| enter(ENTERED + n)).collect());
    world.apply(&tx, &before).expect("entered");
    world.write(
        &tx,
        vec![put(model::unit(Column::Aux, 3), b"kept", b"it is")],
    );
    world.check(&tx);

    // The enters of this batch lie in two leaves of the directory, as
    // `a_batch_of_enters_writes_each_leaf_of_the_directory_once` has
    // them: the first is written, and the second is not.
    let batch = subtrees(names(24).into_iter().map(enter).collect());
    let images = leaf_images_written();
    lignin_tree::flat::fail_leaf_image_after(1);
    let failed = world.apply(&tx, &batch);
    let flat =
        |error: &DirectoryError| matches!(error, DirectoryError::Flat(FlatError::Damaged { .. }));
    assert!(
        matches!(&failed, Err(TxnError::Directory(error)) if flat(error)),
        "{failed:?}"
    );
    // The fault is of the second image, so one leaf of the directory was
    // written before it. The images since are that one and those of the
    // frames, which were made again from the writes before the batch.
    assert!(leaf_images_written() - images >= 1);

    // What was taken before is there, and nothing of the batch is.
    world.check(&tx);
    let entered = |name: u8| {
        let read = tx.txn.subtree(sub(name), |view| view.is_some());
        read.expect("the directory is read")
    };
    assert!((30..36).all(|n| entered(ENTERED + n)));
    assert!(names(24).into_iter().all(|name| !entered(name)));

    // The same batch goes through afterwards, and the whole commits.
    world.apply(&tx, &batch).expect("the batch goes through");
    world.check(&tx);
    assert!(names(24).into_iter().all(entered));
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
}
