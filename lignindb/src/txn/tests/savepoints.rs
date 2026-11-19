//! Savepoints (`crate::savepoint`, `transactions.md` WT8, SC8, T-SP1): the
//! two sentences of the task's acceptance test, each on a store on disk,
//! the cases of section 16 that a savepoint decides, and what a savepoint
//! owes the rules around it: scopes, commit and rollback, page ids and node
//! handles.
//!
//! A root hash is read from the transaction and proved on the way: the walk
//! goes from the root of a Merk to every node below it, and each stored
//! child hash has to be the hash of the child that is there. The grove of
//! these tests is hashed as GroveDB hashes one: the element of a tree holds
//! the root hash of the Merk under it, so the hash of the root Merk stands
//! for every record of every tree.

use std::sync::atomic::Ordering;

use lignin_format::tree_page::{Hash, HashField, Side};
use lignin_tree::{hash::Stale, node::NodeHandle};

use super::{
    model::{self, hash_of, value, Committed, OwnedBatch, OwnedMerk, OwnedSubtree, UNMADE},
    store::{Db, Outcome, Rng, ENTERED},
    *,
};
use crate::savepoint::SavepointError;

/// The Merk at the root of the grove.
const ROOT: u8 = 50;

/// The shortest value that the store of these tests puts out of line.
const LONG: usize = lignin_tree::value::InlinePolicy::DEFAULT_THRESHOLD + 1;

const NO_HASH: Hash = [0; 32];

/// The keys and values of one Merk.
type Items = BTreeMap<Vec<u8>, Vec<u8>>;

fn tree(index: usize) -> u8 {
    60 + index as u8
}

fn element(index: usize) -> Vec<u8> {
    format!("tree-{index:02}").into_bytes()
}

fn key(index: usize) -> Vec<u8> {
    format!("n{index:04}").into_bytes()
}

fn aux() -> Unit {
    model::unit(Column::Aux, 0)
}

fn meta() -> Unit {
    model::unit(Column::Meta, 9)
}

fn get(tx: &Tx<'_>, unit: Unit, key: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let found = tx.txn.get(unit, key, &mut out).expect("a read");
    found.then_some(out)
}

fn committed(outcome: Outcome) -> bool {
    matches!(outcome, Outcome::Committed(_))
}

fn records(records: Vec<Write>) -> OwnedBatch {
    OwnedBatch {
        records,
        ..OwnedBatch::default()
    }
}

/// Whether a change set changes its Merk.
fn changes(merk: &OwnedMerk) -> bool {
    !merk.nodes.is_empty() || !merk.removed.is_empty() || merk.root.is_some()
}

/// The items of the Merk `name` as `tx` reads them, by the model.
fn items(world: &World<'_>, tx: &Tx<'_>, name: u8) -> Items {
    let unit = Unit::data(model::subtree(name));
    Committed::items(&tx.model.borrow().view(&world.model.records, unit))
}

/// The root hash of the Merk that holds `items`, as the model hashes it.
fn hash_of_items(items: &Items) -> Hash {
    let (root, records) = model::records(items);
    let Some(root) = root else {
        return NO_HASH;
    };
    let node = &records[&root];
    let child = |side| node.link(side).map_or(&NO_HASH, |link| &link.hash);
    hash_of(&[&node.kv_hash, child(Side::Left), child(Side::Right)])
}

/// A batch that changes the trees `trees` of the grove, each by `edit`, and
/// the elements that host them in the root Merk, which hold the root hashes
/// of their trees: every write propagates to the root (GM1).
fn grove<'s>(
    world: &World<'s>,
    tx: &Tx<'s>,
    trees: &[usize],
    edit: &mut dyn FnMut(usize, &mut Items),
) -> OwnedBatch {
    let mut merks = Vec::new();
    let mut hashes = Vec::new();
    for index in trees {
        let merk = world.merk(tx, tree(*index), |items| {
            edit(*index, items);
            hashes.push(hash_of_items(items));
        });
        if changes(&merk) {
            merks.push(merk);
        }
    }
    let mut root = world.merk(tx, ROOT, |items| {
        for (index, hash) in trees.iter().zip(&hashes) {
            let mut value = b"tree:".to_vec();
            value.extend_from_slice(hash);
            items.insert(element(*index), value);
        }
    });
    for index in trees {
        (root.hosted).insert(element(*index), vec![model::subtree(tree(*index))]);
    }
    let records = World::root_record(&root).into_iter().collect();
    merks.push(root);
    OwnedBatch {
        merks,
        records,
        ..OwnedBatch::default()
    }
}

/// Changes one tree of the grove through `tx`.
fn change<'s>(world: &World<'s>, tx: &Tx<'s>, index: usize, mut edit: impl FnMut(&mut Items)) {
    let batch = grove(world, tx, &[index], &mut |_, items| edit(items));
    world.apply(tx, &batch).expect("the tree is changed");
}

/// What one read of a node gives the walk.
struct Seen {
    key: Vec<u8>,
    value_hash: Hash,
    kv_hash: Hash,
    /// The stored hash of each child, and the child's key where the node
    /// names it.
    links: [Option<(Hash, Option<Vec<u8>>)>; 2],
    handle: Option<NodeHandle>,
}

fn see(view: NodeView<'_>) -> Seen {
    let link = |side| {
        view.link(side)
            .map(|link| (*link.hash, link.key.map(<[u8]>::to_vec)))
    };
    Seen {
        key: view.key().to_vec(),
        value_hash: *view.value_hash(),
        kv_hash: *view.kv_hash(),
        links: [link(Side::Left), link(Side::Right)],
        handle: view.handle(),
    }
}

/// Where the walk reads a node.
enum At {
    Key(Vec<u8>),
    Child(NodeHandle, Side),
}

/// The hash of the node at `at` in the Merk `subtree`, as `tx` reads it,
/// proved against everything below the node: each child is read where the
/// node says it is, by the page pointer of its cell or by the key its
/// record names, and the hash the node stores for it has to be the child's.
fn proved(tx: &Tx<'_>, subtree: Subtree, at: At, below: &mut BTreeSet<Vec<u8>>) -> Hash {
    let seen = match at {
        At::Key(key) => (tx.txn.node(subtree, &key, |view| view.map(see)))
            .expect("a read")
            .unwrap_or_else(|| panic!("no node under {key:?}")),
        At::Child(parent, side) => (tx.txn)
            .child(subtree, parent, side, |view| view.map(see))
            .expect("a read")
            .expect("the handle is of this walk")
            .expect("a link has a child"),
    };
    assert_eq!(
        seen.kv_hash,
        hash_of(&[&seen.key, &seen.value_hash]),
        "the key-value hash of {:?}",
        seen.key
    );
    let mut children = [NO_HASH; 2];
    for (index, side) in [Side::Left, Side::Right].into_iter().enumerate() {
        let Some((stored, child)) = &seen.links[index] else {
            continue;
        };
        let at = match (child, seen.handle) {
            (Some(child), _) => At::Key(child.clone()),
            (None, Some(handle)) => At::Child(handle, side),
            (None, None) => panic!("a link of {:?} leads nowhere", seen.key),
        };
        children[index] = proved(tx, subtree, at, below);
        assert_eq!(*stored, children[index], "a child hash of {:?}", seen.key);
    }
    let hash = hash_of(&[&seen.kv_hash, &children[0], &children[1]]);
    assert!(below.insert(seen.key), "a node is reached twice");
    hash
}

/// Checks that `tx` reads nothing in `unit`: no flat record, and in a data
/// column no node of a Merk.
fn assert_empty(tx: &Tx<'_>, unit: Unit) {
    assert_eq!(scan(&mut tx.txn.cursor(unit), false), Vec::new(), "{unit}");
    if unit.column == Column::Data {
        let mut cursor = tx.txn.merk_cursor(unit.subtree).expect("a cursor");
        cursor.seek_to_first().expect("a seek");
        assert_eq!(cursor.key(), None, "a node in {unit}");
    }
}

/// The keys of the nodes of the Merk `subtree` as a cursor of `tx` shows
/// them.
fn shown(tx: &Tx<'_>, subtree: Subtree) -> BTreeSet<Vec<u8>> {
    let mut cursor = tx.txn.merk_cursor(subtree).expect("a cursor");
    cursor.seek_to_first().expect("a seek");
    let mut keys = BTreeSet::new();
    while let Some(key) = cursor.key() {
        keys.insert(key.to_vec());
        cursor.next().expect("a step");
    }
    keys
}

/// The root hash of the Merk `name` as `tx` reads it.
///
/// The nodes of the Merk are the ones a cursor of `tx` shows, and they are
/// the model's. The walk starts at the key the model has for the root and
/// has to reach every one of them: the node it starts at is then the root
/// of the Merk as the transaction has it, whoever named it. A Merk with no
/// node is read as empty.
fn root_hash(world: &World<'_>, tx: &Tx<'_>, name: u8) -> Hash {
    let subtree = model::subtree(name);
    let items = items(world, tx, name);
    let nodes = shown(tx, subtree);
    assert!(
        nodes.iter().eq(items.keys()),
        "the nodes of the Merk {name} that the transaction shows"
    );
    let Some(root) = model::records(&items).0 else {
        assert_empty(tx, Unit::data(subtree));
        return NO_HASH;
    };
    let mut below = BTreeSet::new();
    let hash = proved(tx, subtree, At::Key(root), &mut below);
    assert_eq!(below, nodes, "every node of the Merk is below its root");
    hash
}

/// The root hash of the grove as `tx` reads it: the root hash of the root
/// Merk, with the root hash of every tree compared with the one its
/// element holds. The elements are the ones `tx` shows in the root Merk,
/// with the values it shows. A tree that no element names is not found
/// here: a rollback is checked for one where only its writes reached
/// ([`Set::roll_back`]), and a commit by the walk of every directory entry
/// (`World::check_committed`).
fn grove_hash(world: &World<'_>, tx: &Tx<'_>) -> Hash {
    let root = model::subtree(ROOT);
    for element in shown(tx, root) {
        let Some(index) = (element.strip_prefix(b"tree-"))
            .and_then(|index| std::str::from_utf8(index).ok()?.parse::<usize>().ok())
        else {
            continue;
        };
        let held = |view: Option<NodeView<'_>>| {
            let mut buf = lignin_tree::value::ValueBuf::new();
            let value = view.expect("an element").value(&mut buf);
            value.expect("the value of an element")[5..].to_vec()
        };
        let held = tx.txn.node(root, &element, held).expect("a read");
        assert_eq!(
            root_hash(world, tx, tree(index))[..],
            held[..],
            "the root hash of tree {index}, which its element holds"
        );
    }
    root_hash(world, tx, ROOT)
}

/// The savepoints a test set on one transaction, as the model has them: the
/// number of the transaction's writes at each.
#[derive(Default)]
struct Set(Vec<usize>);

impl Set {
    fn set(&mut self, tx: &Tx<'_>) {
        tx.txn.set_savepoint().expect("no scope is open");
        self.0.push(tx.model.borrow().writes.len());
        assert_eq!(tx.txn.savepoints(), self.0.len());
    }

    /// Rolls `tx` back to its newest savepoint, and reads nothing.
    fn roll_back_unread(&mut self, tx: &Tx<'_>) {
        (tx.txn.rollback_to_savepoint()).expect("a savepoint is set");
        let writes = self.0.pop().expect("the model has the savepoint");
        tx.model.borrow_mut().writes.truncate(writes);
        assert_eq!(tx.txn.savepoints(), self.0.len());
    }

    /// Rolls `tx` back to its newest savepoint, and checks that it reads
    /// nothing in the units that only the rolled-back writes named: a
    /// check of the units the model has left would not look there.
    /// Returns how many those were.
    fn roll_back<'s>(&mut self, world: &World<'s>, tx: &Tx<'s>) -> usize {
        let before = world.units(tx);
        self.roll_back_unread(tx);
        let now = world.units(tx);
        let gone = before.difference(&now);
        gone.map(|unit| assert_empty(tx, *unit)).count()
    }

    fn pop(&mut self, tx: &Tx<'_>) {
        tx.txn.pop_savepoint().expect("a savepoint is set");
        self.0.pop().expect("the model has the savepoint");
        assert_eq!(tx.txn.savepoints(), self.0.len());
    }
}

/// The tree of the grove that [`planted`] enters under an element of its
/// own, and the Merk that a node of that tree hosts.
const OUTER: usize = (ENTERED - 60) as usize + 10;
const INNER: u8 = ENTERED + 11;

/// The flat records beside the Merk [`INNER`], in two columns.
fn inner(column: Column) -> Unit {
    model::unit(column, INNER)
}

/// A tree that no store has entered.
const NEW: usize = (ENTERED - 60) as usize + 12;

/// A committed grove of eleven trees, and flat records beside it. One
/// value in five is out of line, and so pending in the committed state:
/// its bytes are with the frame of its page (FL7). Ten of the trees are
/// among the Merks every store of these tests is made with. The eleventh,
/// [`OUTER`], is entered here under its element, and a node of it hosts
/// the Merk [`INNER`], which has flat records beside it.
fn planted(world: &mut World<'_>, rng: &mut Rng) {
    let tx = world.begin();
    let mut all: Vec<usize> = (0..10).collect();
    all.push(OUTER);
    let mut batch = grove(world, &tx, &all, &mut |index, items| {
        for node in 0..3 + (index % 10) * 6 {
            let len = match node % 5 {
                0 => LONG + rng.below(600),
                _ => 10 + rng.below(120),
            };
            items.insert(key(node * 3), value(rng, len));
        }
        if index == OUTER {
            items.insert(b"host".to_vec(), b"tree".to_vec());
        }
    });
    let outer = model::subtree(tree(OUTER));
    for merk in &mut batch.merks {
        if merk.subtree == outer {
            (merk.hosted).insert(b"host".to_vec(), vec![model::subtree(INNER)]);
        }
    }
    batch.merks.insert(
        0,
        world.merk(&tx, INNER, |items| {
            for node in 0..12 {
                items.insert(key(node), value(rng, 30 + node * 40));
            }
        }),
    );
    batch.subtrees = vec![
        OwnedSubtree::Enter(outer, Some((model::subtree(ROOT), element(OUTER), 0))),
        OwnedSubtree::Enter(model::subtree(INNER), Some((outer, b"host".to_vec(), 0))),
    ];
    world.apply(&tx, &batch).expect("the grove is made");
    let mut records = Vec::new();
    for index in 0..40 {
        let len = 1 + rng.below(80);
        records.push(put(aux(), &key(index), &value(rng, len)));
        records.push(put(meta(), &key(index), &value(rng, len)));
    }
    records.push(put(inner(Column::Aux), b"below", b"an aux record"));
    records.push(put(inner(Column::Meta), b"below", b"a meta record"));
    world.write(&tx, records);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

/// Drops the tree [`OUTER`] through `tx`, and deletes the element that
/// hosts it. The Merk below it goes with it, which the drop does not name.
fn drop_outer<'s>(world: &World<'s>, tx: &Tx<'s>) {
    let root = world.merk(tx, ROOT, |items| {
        items.remove(&element(OUTER));
    });
    let batch = OwnedBatch {
        records: World::root_record(&root).into_iter().collect(),
        merks: vec![root],
        subtrees: vec![OwnedSubtree::Drop(model::subtree(tree(OUTER)))],
    };
    world.apply(tx, &batch).expect("the tree is dropped");
}

/// Enters the tree `index` under an element of its own, with the nodes
/// `edit` gives it.
fn enter<'s>(world: &World<'s>, tx: &Tx<'s>, index: usize, mut edit: impl FnMut(&mut Items)) {
    let mut batch = grove(world, tx, &[index], &mut |_, items| edit(items));
    let host = (model::subtree(ROOT), element(index), 0);
    let entered = OwnedSubtree::Enter(model::subtree(tree(index)), Some(host));
    batch.subtrees.push(entered);
    world.apply(tx, &batch).expect("the tree is entered");
}

/// Clears the tree `index` as a whole, and writes its element, which hosts
/// an empty Merk then.
fn clear<'s>(world: &World<'s>, tx: &Tx<'s>, index: usize) {
    let subtree = model::subtree(tree(index));
    let mut batch = grove(world, tx, &[index], &mut |_, items| items.clear());
    batch.merks.retain(|merk| merk.subtree != subtree);
    batch.subtrees.push(OwnedSubtree::Clear(subtree));
    world.apply(tx, &batch).expect("the tree is cleared");
}

/// Checks that a change set for the subtree `name` is refused: `tx` reads
/// no subtree of that name.
fn assert_not_entered<'s>(world: &World<'s>, tx: &Tx<'s>, name: u8) {
    let merk = world.merk(tx, name, |items| {
        items.insert(b"n".to_vec(), b"v".to_vec());
    });
    let batch = OwnedBatch {
        merks: vec![merk],
        ..OwnedBatch::default()
    };
    let error = world.apply(tx, &batch).expect_err("no such subtree");
    assert!(
        matches!(error, TxnError::Batch(BatchError::NotEntered { subtree }) if subtree == model::subtree(name)),
        "{error}"
    );
}

/// The first sentence of the acceptance test. Between the savepoint and the
/// rollback the transaction changes values in their cells, out of line and
/// pending from an earlier commit, cuts and joins pages, gives Merks other
/// roots, empties one and fills one, drops a subtree with the Merk below
/// it and the flat records beside that, enters a new one, clears a third,
/// and writes flat records.
#[test]
fn rolling_back_to_a_savepoint_restores_the_exact_root_hash_it_had() {
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(404);
    planted(&mut world, &mut rng);
    let committed_hash = grove_hash(&world, &world.begin());

    let tx = world.begin();
    let mut set = Set::default();
    // What the transaction wrote before the savepoint stays: values in two
    // trees, some of them out of line, and flat records, one of them beside
    // the Merk that a later drop takes from above.
    change(&world, &tx, 2, |items| {
        items.insert(key(1), value(&mut rng, LONG + 900));
        items.insert(key(6), b"replaced before the savepoint".to_vec());
        items.remove(&key(9));
    });
    change(&world, &tx, 7, |items| {
        for node in 100..130 {
            items.insert(key(node), value(&mut rng, 60));
        }
    });
    world.write(
        &tx,
        vec![
            put(aux(), b"before", b"kept"),
            delete(meta(), &key(3)),
            put(inner(Column::Aux), b"own", b"written before the savepoint"),
        ],
    );
    let below = |tx: &Tx<'_>| {
        [
            get(tx, inner(Column::Aux), b"below"),
            get(tx, inner(Column::Meta), b"below"),
            get(tx, inner(Column::Aux), b"own"),
        ]
    };
    let saved_below = below(&tx);
    assert!(saved_below.iter().all(Option::is_some));
    let saved = grove_hash(&world, &tx);
    assert_ne!(saved, committed_hash);
    let saved_inner = root_hash(&world, &tx, INNER);
    let held = tx.txn.stats();
    set.set(&tx);

    // Values: replaced in the cell, moved out of line and back, and the
    // value from before the savepoint replaced again. Of the values that
    // are pending from the commit that planted the grove, one is replaced
    // by another out of line, one by one in its cell, and one goes with
    // its node; the others are kept by nodes that are written around them.
    let pending = |tx: &Tx<'_>| {
        let long = |value: &Vec<u8>| value.len() >= LONG;
        [key(0), key(15), key(30)].map(|key| items(&world, tx, tree(2)).get(&key).is_some_and(long))
    };
    assert_eq!(pending(&tx), [true; 3]);
    change(&world, &tx, 2, |items| {
        items.insert(key(1), b"short now".to_vec());
        items.insert(key(0), value(&mut rng, LONG + 2_000));
        items.insert(key(6), value(&mut rng, LONG));
        items.insert(key(15), b"in its cell now".to_vec());
        items.remove(&key(30));
    });
    assert_ne!(grove_hash(&world, &tx), saved);
    // Structure: a tree grows over many pages, and another loses most of
    // its nodes, so pages are cut and joined and cells are carried.
    change(&world, &tx, 4, |items| {
        for node in 0..400 {
            let len = 30 + rng.below(200);
            items.insert(key(1_000 + node), value(&mut rng, len));
        }
    });
    change(&world, &tx, 9, |items| {
        let keys: Vec<Vec<u8>> = items.keys().cloned().collect();
        for key in keys.iter().skip(2) {
            items.remove(key);
        }
    });
    world.check(&tx);
    // The directory: a tree is emptied, a new one is made, one gets another
    // root, and the root Merk grows until hosts are carried with the roots
    // under them.
    change(&world, &tx, 1, Items::clear);
    change(&world, &tx, 12, |items| {
        for node in 0..25 {
            items.insert(key(node), value(&mut rng, 50));
        }
    });
    change(&world, &tx, 0, |items| {
        let root = model::records(items).0.expect("the tree has nodes");
        items.remove(&root);
    });
    let mut carried = 0;
    for round in 0..5 {
        let applied = world.edit(&tx, ROOT, |items| {
            for index in 0..40 {
                let key = format!("tree-{:02}-{round}-{index}", rng.below(13));
                items.insert(key.into_bytes(), value(&mut rng, 150));
            }
        });
        carried += applied.carried.len();
    }
    assert!(carried > 0, "cells of the root Merk were carried");
    // Subtrees. One that is committed is dropped with the Merk below it,
    // and its host deleted. A new one is entered under a new element, and
    // filled. A third is cleared as a whole, and filled again.
    drop_outer(&world, &tx);
    world.check(&tx);
    assert_not_entered(&world, &tx, INNER);
    // While the drop stands, no flat record beside the Merk below is read
    // (CV9): not the committed ones, and not the transaction's own.
    assert_eq!(below(&tx), [None, None, None]);
    enter(&world, &tx, NEW, |items| {
        for node in 0..60 {
            items.insert(key(node), value(&mut rng, 40 + node * 9));
        }
    });
    clear(&world, &tx, 3);
    assert_eq!(root_hash(&world, &tx, tree(3)), NO_HASH);
    change(&world, &tx, 3, |items| {
        for node in 0..20 {
            items.insert(key(node * 2), value(&mut rng, 70));
        }
    });
    // Flat records, in three columns.
    world.write(
        &tx,
        vec![
            put(aux(), b"before", b"changed"),
            put(aux(), b"after", b"new"),
            Write::Range(meta(), key(10), Some(key(30))),
            put(model::unit(Column::Roots, 8), b"r", b"a root record"),
        ],
    );
    world.check(&tx);
    let changed = tx.txn.stats();
    assert!(changed.writes > held.writes + 600);
    assert!(changed.tree_pages > held.tree_pages + 8);
    assert_ne!(grove_hash(&world, &tx), saved);

    let emptied = set.roll_back(&world, &tx);
    assert_eq!(
        emptied, 3,
        "the tree that was filled, the one that was entered and the root record: nothing is read there"
    );
    assert_eq!(
        grove_hash(&world, &tx),
        saved,
        "the root hash the grove had at the savepoint"
    );
    // The subtree that was dropped is there again with the Merk below it,
    // and the one that was entered is entered no more.
    assert_eq!(root_hash(&world, &tx, INNER), saved_inner);
    assert_ne!(saved_inner, NO_HASH);
    assert_eq!(below(&tx), saved_below, "the flat records beside it");
    assert_not_entered(&world, &tx, tree(NEW));
    assert_eq!(pending(&tx), [true; 3]);
    // Every record, every cursor and every committed read is what the
    // writes from before the savepoint make it.
    world.check(&tx);
    let back = tx.txn.stats();
    assert_eq!(
        (back.writes, back.write_set_bytes, back.overlapped),
        (held.writes, held.write_set_bytes, 0)
    );
    assert_eq!(
        (back.tree_pages, back.flat_pages),
        (held.tree_pages, held.flat_pages)
    );
    assert_eq!(back.value_pages, held.value_pages);
    assert_eq!(tx.txn.savepoints(), 0);

    // The transaction goes on from there, and what it commits has the hash
    // it shows: nothing of the rolled-back writes is in the pages.
    change(&world, &tx, 4, |items| {
        items.insert(key(5_000), b"after the rollback".to_vec());
    });
    let last = grove_hash(&world, &tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    assert_eq!(grove_hash(&world, &world.begin()), last);
    assert_eq!(db.private_ids(), 0);
}

/// A savepoint that is set before the transaction's first write: the
/// rollback leaves a transaction that holds nothing, and it reads the
/// committed grove.
#[test]
fn a_rollback_to_a_savepoint_of_an_empty_write_set_leaves_nothing_held() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(405);
    planted(&mut world, &mut rng);
    let tx = world.begin();
    let saved = grove_hash(&world, &tx);
    let mut set = Set::default();
    set.set(&tx);
    change(&world, &tx, 3, |items| {
        for node in 0..120 {
            items.insert(key(500 + node), value(&mut rng, LONG + 100));
        }
    });
    world.write(&tx, vec![put(aux(), b"a", b"1")]);
    assert_ne!(grove_hash(&world, &tx), saved);
    assert!(db.private_ids() > 0);

    set.roll_back_unread(&tx);
    let stats = tx.txn.stats();
    assert_eq!(
        (
            stats.writes,
            stats.pinned,
            stats.tree_pages,
            stats.flat_pages
        ),
        (0, None, 0, 0),
        "it holds nothing again"
    );
    assert_eq!(db.private_ids(), 0);
    assert_eq!(db.pool.census().private, 0);
    assert_eq!(grove_hash(&world, &tx), saved);
    world.check(&tx);
    assert_eq!(world.commit(tx, &[]), Outcome::Empty);
}

/// One round of changes to the grove: values, nodes that come and go, a
/// tree that is made or emptied, and flat records.
fn round<'s>(world: &World<'s>, tx: &Tx<'s>, rng: &mut Rng, level: usize) {
    change(world, tx, level % 10, |items| {
        for node in 0..40 {
            let len = match node % 4 {
                0 => LONG + rng.below(400),
                _ => 20 + rng.below(100),
            };
            items.insert(key(2_000 + level * 50 + node), value(rng, len));
        }
        let first = items.keys().next().cloned().expect("a node");
        items.remove(&first);
    });
    match level % 2 {
        0 => change(world, tx, 20 + level, |items| {
            items.insert(key(level), value(rng, 80));
        }),
        _ => change(world, tx, (level + 5) % 10, Items::clear),
    }
    let name = format!("level-{level}").into_bytes();
    world.write(
        tx,
        vec![put(aux(), &name, b"set"), delete(meta(), &key(level))],
    );
}

/// The second sentence of the acceptance test: savepoints are set inside
/// one another, and each rollback goes to the newest one that is left.
#[test]
fn nested_savepoints_work() {
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(406);
    planted(&mut world, &mut rng);
    let tx = world.begin();
    let mut set = Set::default();
    let state = |tx: &Tx<'_>| (grove_hash(&world, tx), tx.txn.stats().writes);
    // Five savepoints, each inside the one before, with the root hash and
    // the write set of the moment it was set.
    let mut saved = Vec::new();
    for level in 0..5 {
        saved.push(state(&tx));
        set.set(&tx);
        round(&world, &tx, &mut rng, level);
        assert_ne!(grove_hash(&world, &tx), saved[level].0);
    }
    assert_eq!(tx.txn.savepoints(), 5);
    world.check(&tx);

    // The two innermost are rolled back, one after the other.
    for level in [4, 3] {
        set.roll_back(&world, &tx);
        assert_eq!(
            state(&tx),
            saved[level],
            "the root hash of savepoint {level}"
        );
        world.check(&tx);
    }
    assert_eq!(tx.txn.savepoints(), 3);

    // New savepoints are set inside the third, and rolled back again.
    let inner = saved[3];
    for level in 5..8 {
        saved.push(state(&tx));
        set.set(&tx);
        round(&world, &tx, &mut rng, level);
    }
    assert_eq!(tx.txn.savepoints(), 6);
    for level in [7, 6, 5] {
        set.roll_back(&world, &tx);
        assert_eq!(state(&tx), saved[level]);
        world.check(&tx);
    }
    assert_eq!(grove_hash(&world, &tx), inner.0);

    // A pop undoes nothing, and its writes belong to the savepoint below:
    // the next rollback takes them out with that savepoint's own.
    set.set(&tx);
    round(&world, &tx, &mut rng, 8);
    set.set(&tx);
    round(&world, &tx, &mut rng, 9);
    let both = state(&tx);
    set.pop(&tx);
    assert_eq!(state(&tx), both);
    world.check(&tx);
    set.roll_back(&world, &tx);
    assert_eq!(state(&tx), inner);
    world.check(&tx);

    // The outer three, down to the write set the transaction began with.
    for level in [2, 1, 0] {
        set.roll_back(&world, &tx);
        assert_eq!(state(&tx), saved[level]);
        world.check(&tx);
    }
    assert_eq!((tx.txn.savepoints(), tx.txn.stats().writes), (0, 0));

    // Case A17: with no savepoint set, a rollback to one and a pop are
    // refused and change nothing.
    round(&world, &tx, &mut rng, 10);
    let before = (grove_hash(&world, &tx), tx.txn.stats());
    assert_eq!(
        tx.txn.rollback_to_savepoint(),
        Err(SavepointError::NoSavepoint)
    );
    assert_eq!(tx.txn.pop_savepoint(), Err(SavepointError::NoSavepoint));
    assert_eq!(tx.txn.stats(), before.1);
    assert_eq!(grove_hash(&world, &tx), before.0);
    world.check(&tx);

    // Platform's use (P-12): a savepoint before each state transition, a
    // rollback and a new savepoint where one fails, and the savepoints of
    // the ones that went through left on the stack.
    let mut kept = 0;
    for transition in 0..12 {
        set.set(&tx);
        let good = grove_hash(&world, &tx);
        round(&world, &tx, &mut rng, 11 + transition);
        if transition % 3 == 0 {
            set.roll_back(&world, &tx);
            assert_eq!(grove_hash(&world, &tx), good);
        } else {
            kept += 1;
        }
    }
    assert_eq!(tx.txn.savepoints(), kept);
    world.check(&tx);
    let last = grove_hash(&world, &tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    assert_eq!(grove_hash(&world, &world.begin()), last);
    assert_eq!(db.private_ids(), 0);
}

/// Savepoints around the life of one subtree: it is entered, dropped with
/// what it holds, entered again and cleared, each after a savepoint of its
/// own, and each rollback gives the subtree of the savepoint back: its
/// nodes with the root hash they had, the records beside them, or no
/// subtree at all.
#[test]
fn nested_savepoints_take_back_an_enter_a_drop_and_a_clear() {
    const S: u8 = ENTERED + 20;
    /// Gives the subtree thirty nodes from `from` on, some with a value out
    /// of line, and a record beside them.
    fn fill<'s>(
        world: &World<'s>,
        tx: &Tx<'s>,
        rng: &mut Rng,
        from: usize,
        subtrees: Vec<OwnedSubtree>,
    ) {
        let merk = world.merk(tx, S, |items| {
            for node in from..from + 30 {
                let len = match node % 4 {
                    0 => LONG + rng.below(300),
                    _ => 20 + rng.below(80),
                };
                items.insert(key(node), value(rng, len));
            }
        });
        let batch = OwnedBatch {
            merks: vec![merk],
            records: vec![put(beside(), &key(from), b"beside")],
            subtrees,
        };
        world.apply(tx, &batch).expect("the nodes are written");
    }
    fn beside() -> Unit {
        model::unit(Column::Aux, S)
    }
    /// A batch that does one thing to one subtree, and nothing else.
    fn alone<'s>(world: &World<'s>, tx: &Tx<'s>, subtree: OwnedSubtree) {
        let batch = OwnedBatch {
            subtrees: vec![subtree],
            ..OwnedBatch::default()
        };
        world.apply(tx, &batch).expect("the batch is applied");
    }
    let census = |db: &Db| *db.counts.census.lock().expect("a census");

    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(410);
    world.check_committed();
    let before = census(&db);
    let subtree = model::subtree(S);
    let enter = || vec![OwnedSubtree::Enter(subtree, None)];

    let (tx, mut set) = (world.begin(), Set::default());
    world.write(
        &tx,
        vec![put(aux(), b"kept", b"from before every savepoint")],
    );
    assert_not_entered(&world, &tx, S);

    // An enter with nothing beside it adds no write to the write set, and
    // is an operation of it: a rollback takes it back, with what was
    // written into the subtree or with nothing.
    const BARE: u8 = ENTERED + 21;
    let bare = |world: &World<'_>, tx: &Tx<'_>| items(world, tx, BARE).len();
    set.set(&tx);
    alone(&world, &tx, OwnedSubtree::Enter(model::subtree(BARE), None));
    assert_eq!(tx.txn.stats().writes, 1);
    set.set(&tx);
    world.edit(&tx, BARE, |items| {
        items.insert(b"n".to_vec(), b"v".to_vec());
    });
    assert_eq!(bare(&world, &tx), 1);
    set.roll_back(&world, &tx);
    assert_eq!(root_hash(&world, &tx, BARE), NO_HASH);
    world.edit(&tx, BARE, |items| {
        items.insert(b"m".to_vec(), b"it is entered still".to_vec());
    });
    set.roll_back(&world, &tx);
    assert_not_entered(&world, &tx, BARE);
    set.set(&tx);
    alone(&world, &tx, OwnedSubtree::Enter(model::subtree(BARE), None));
    set.roll_back(&world, &tx);
    assert_not_entered(&world, &tx, BARE);
    assert_eq!((bare(&world, &tx), tx.txn.stats().writes), (0, 1));

    // Entered and filled.
    set.set(&tx);
    fill(&world, &tx, &mut rng, 0, enter());
    let first = root_hash(&world, &tx, S);
    world.check(&tx);

    // Dropped: nothing is read under it, and it takes no change set.
    set.set(&tx);
    alone(&world, &tx, OwnedSubtree::Drop(subtree));
    assert_eq!(root_hash(&world, &tx, S), NO_HASH);
    assert_eq!(get(&tx, beside(), &key(0)), None);
    assert_not_entered(&world, &tx, S);
    world.check(&tx);

    // Entered again, it starts empty and gets other nodes.
    set.set(&tx);
    fill(&world, &tx, &mut rng, 100, enter());
    let again = root_hash(&world, &tx, S);
    assert_ne!(again, first);
    assert_eq!(items(&world, &tx, S).len(), 30);
    world.check(&tx);

    // Back to the dropped subtree, and then to the one that was filled
    // first.
    set.roll_back(&world, &tx);
    assert_eq!(root_hash(&world, &tx, S), NO_HASH);
    assert_not_entered(&world, &tx, S);
    world.check(&tx);
    set.roll_back(&world, &tx);
    assert_eq!(root_hash(&world, &tx, S), first);
    assert_eq!(get(&tx, beside(), &key(0)), Some(b"beside".to_vec()));
    world.check(&tx);

    // Cleared: the nodes go and the record beside them stays. It is
    // filled again, and the rollback gives the first nodes back.
    set.set(&tx);
    alone(&world, &tx, OwnedSubtree::Clear(subtree));
    assert_eq!(root_hash(&world, &tx, S), NO_HASH);
    assert_eq!(get(&tx, beside(), &key(0)), Some(b"beside".to_vec()));
    fill(&world, &tx, &mut rng, 200, Vec::new());
    world.check(&tx);
    set.roll_back(&world, &tx);
    assert_eq!(root_hash(&world, &tx, S), first);
    world.check(&tx);

    // Back before it was entered: there is no such subtree, and what the
    // transaction commits leaves the directory as it was.
    let emptied = set.roll_back(&world, &tx);
    assert_eq!(
        emptied, 2,
        "the nodes of the subtree and the record beside them"
    );
    assert_not_entered(&world, &tx, S);
    assert_eq!((tx.txn.savepoints(), tx.txn.stats().writes), (0, 1));
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    assert_eq!(census(&db), before);
    assert!(!db
        .roots(&versions, versions.latest())
        .contains_key(&subtree));
    assert_eq!(db.private_ids(), 0);
}

/// T-SP1, the cases of 16.3: A4, A4b, A5, A5b, A16 and A17, on flat keys.
/// `W` is a write with no transaction.
#[test]
fn the_savepoint_cases_of_the_conflict_rule_on_flat_keys() {
    let (db, versions) = Db::new(48);
    let mut world = World::new(&db, &versions);

    // A4, A4b: T sets a savepoint; puts K; W puts K; T rolls back to it. It
    // reads W's value, not what was there when the savepoint was set, and
    // commits.
    let (tx, mut set) = (world.begin(), Set::default());
    set.set(&tx);
    world.write(&tx, vec![put(aux(), b"a4", b"t")]);
    world.write_outside(&records(vec![put(aux(), b"a4", b"w")]), &[&tx]);
    assert_eq!(get(&tx, aux(), b"a4"), Some(b"t".to_vec()));
    assert_eq!(tx.txn.stats().overlapped, 1);
    set.roll_back(&world, &tx);
    assert_eq!(get(&tx, aux(), b"a4"), Some(b"w".to_vec()));
    world.check(&tx);
    assert_eq!(world.commit(tx, &[]), Outcome::Empty);

    // A4 with a write of another key from before the savepoint, so that
    // the commit has a write set to decide on: no write of it is met, and
    // it publishes.
    let (tx, mut set) = (world.begin(), Set::default());
    world.write(&tx, vec![put(aux(), b"a4-kept", b"t")]);
    set.set(&tx);
    world.write(&tx, vec![put(aux(), b"a4-met", b"t")]);
    world.write_outside(&records(vec![put(aux(), b"a4-met", b"w")]), &[&tx]);
    set.roll_back(&world, &tx);
    assert!(committed(world.commit(tx, &[])));
    let reader = world.begin();
    assert_eq!(get(&reader, aux(), b"a4-met"), Some(b"w".to_vec()));
    assert_eq!(get(&reader, aux(), b"a4-kept"), Some(b"t".to_vec()));

    // A5: T puts K; sets a savepoint; puts K; W puts K; T rolls back to it.
    // The write from before the savepoint stays, with its first-write
    // version.
    let (tx, mut set) = (world.begin(), Set::default());
    world.write(&tx, vec![put(aux(), b"a5", b"t")]);
    set.set(&tx);
    world.write(&tx, vec![put(aux(), b"a5", b"t again")]);
    world.write_outside(&records(vec![put(aux(), b"a5", b"w")]), &[&tx]);
    set.roll_back(&world, &tx);
    assert_eq!(get(&tx, aux(), b"a5"), Some(b"t".to_vec()));
    world.check(&tx);
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);
    assert_eq!(get(&world.begin(), aux(), b"a5"), Some(b"w".to_vec()));

    // A5b: T sets a savepoint; puts K; W puts K; T rolls back; puts K;
    // commits. K holds T's second value.
    let (tx, mut set) = (world.begin(), Set::default());
    world.write(&tx, vec![put(aux(), b"other", b"kept")]);
    set.set(&tx);
    world.write(&tx, vec![put(aux(), b"a5b", b"t")]);
    world.write_outside(&records(vec![put(aux(), b"a5b", b"w")]), &[&tx]);
    assert_eq!(get(&tx, aux(), b"a5b"), Some(b"t".to_vec()));
    assert!(tx.txn.stats().overlapped > 0);
    set.roll_back(&world, &tx);
    world.write(&tx, vec![put(aux(), b"a5b", b"t again")]);
    assert_eq!(tx.txn.stats().overlapped, 0);
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    assert_eq!(
        get(&world.begin(), aux(), b"a5b"),
        Some(b"t again".to_vec())
    );

    // A16: T sets savepoint 1; puts K; sets savepoint 2; puts K2; W puts K;
    // T rolls back to 2. K is from before it.
    let (tx, mut set) = (world.begin(), Set::default());
    set.set(&tx);
    world.write(&tx, vec![put(aux(), b"a16", b"t")]);
    set.set(&tx);
    world.write(&tx, vec![put(aux(), b"a16-2", b"t")]);
    world.write_outside(&records(vec![put(aux(), b"a16", b"w")]), &[&tx]);
    set.roll_back(&world, &tx);
    assert_eq!(get(&tx, aux(), b"a16-2"), None);
    assert_eq!(tx.txn.savepoints(), 1);
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);

    // A16 on: the rollback to savepoint 1 takes K out as well, and the
    // transaction commits what it writes then.
    let (tx, mut set) = (world.begin(), Set::default());
    set.set(&tx);
    world.write(&tx, vec![put(aux(), b"a16", b"t")]);
    set.set(&tx);
    world.write(&tx, vec![put(aux(), b"a16-2", b"t")]);
    world.write_outside(&records(vec![put(aux(), b"a16", b"w again")]), &[&tx]);
    set.roll_back(&world, &tx);
    set.roll_back(&world, &tx);
    assert_eq!(get(&tx, aux(), b"a16"), Some(b"w again".to_vec()));
    world.write(&tx, vec![put(aux(), b"a16-3", b"t")]);
    assert!(committed(world.commit(tx, &[])));

    // A17: T rolls back to a savepoint when none is set.
    let tx = world.begin();
    world.write(&tx, vec![put(aux(), b"a17", b"t")]);
    let before = tx.txn.stats();
    let error = tx.txn.rollback_to_savepoint().expect_err("none is set");
    assert_eq!(error, SavepointError::NoSavepoint);
    assert_eq!(error.to_string(), "the transaction has no savepoint");
    assert!(std::error::Error::source(&error).is_none());
    assert_eq!(tx.txn.stats(), before);
    assert_eq!(get(&tx, aux(), b"a17"), Some(b"t".to_vec()));
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

/// Inserts one item into a tree of the grove.
fn insert<'s>(world: &World<'s>, tx: &Tx<'s>, tree: usize, name: &[u8]) {
    change(world, tx, tree, |items| {
        items.insert(name.to_vec(), b"an item".to_vec());
    });
}

/// An insert with no transaction.
fn outside<'s>(world: &mut World<'s>, tree: usize, name: &[u8], others: &[&Tx<'s>]) {
    let tx = world.begin();
    insert(world, &tx, tree, name);
    assert!(committed(world.commit(tx, others)));
}

const A: usize = 0;
const B: usize = 1;

/// The state every case of 16.4 starts from: the two empty trees `A` and
/// `B` under the root of the grove.
fn two_empty_trees(world: &mut World<'_>) {
    let tx = world.begin();
    let batch = grove(world, &tx, &[A, B], &mut |_, _| {});
    world.apply(&tx, &batch).expect("the trees are made");
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    let reader = world.begin();
    assert!(items(world, &reader, tree(A)).is_empty() && items(world, &reader, tree(B)).is_empty());
    assert_eq!(items(world, &reader, ROOT).len(), 2);
}

/// T-SP1, case B6 of 16.4: *T* sets a savepoint; inserts `A/a1`; None
/// inserts `B/b1`; *T* rolls back to it; inserts `A/a2`; commits. Every
/// insert writes the root Merk (GM1), so a write to one tree meets a write
/// to the other (GM2), and the savepoint decides by what remains (GM6):
/// every tree change *T* made before the commit outside is rolled back,
/// and it commits. "None" is a write with no transaction.
#[test]
fn case_b6_a_transaction_commits_when_its_met_writes_are_rolled_back() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    two_empty_trees(&mut world);

    let (tx, mut set) = (world.begin(), Set::default());
    set.set(&tx);
    insert(&world, &tx, A, b"a1");
    outside(&mut world, B, b"b1", &[&tx]);
    // Until the rollback the transaction serves its records as they are
    // (WT7b): its root Merk does not have the element None wrote.
    world.check(&tx);
    assert!(tx.txn.stats().overlapped > 0);
    set.roll_back(&world, &tx);
    assert_eq!(tx.txn.stats().overlapped, 0);
    let outside_hash = grove_hash(&world, &world.begin());
    assert_eq!(
        grove_hash(&world, &tx),
        outside_hash,
        "it reads the grove as None left it"
    );
    insert(&world, &tx, A, b"a2");
    world.check(&tx);
    let last = grove_hash(&world, &tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    let reader = world.begin();
    assert_eq!(grove_hash(&world, &reader), last);
    let keys = |tree: usize| -> Vec<Vec<u8>> {
        items(&world, &reader, self::tree(tree))
            .into_keys()
            .collect()
    };
    assert_eq!(keys(A), [b"a2".to_vec()]);
    assert_eq!(keys(B), [b"b1".to_vec()]);
    assert_eq!(db.private_ids(), 0);
}

/// T-SP1, case B7 of 16.4: *T* inserts `A/a0`; sets a savepoint; inserts
/// `A/a1`; None inserts `B/b1`; *T* rolls back to it; commits. A tree
/// change from before the savepoint remains, and the commit fails (GM6).
#[test]
fn case_b7_a_transaction_conflicts_when_a_met_write_from_before_the_savepoint_remains() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    two_empty_trees(&mut world);

    let (tx, mut set) = (world.begin(), Set::default());
    insert(&world, &tx, A, b"a0");
    set.set(&tx);
    insert(&world, &tx, A, b"a1");
    outside(&mut world, B, b"b1", &[&tx]);
    set.roll_back(&world, &tx);
    world.check(&tx);
    assert!(
        tx.txn.stats().overlapped > 0,
        "the write from before the savepoint is met"
    );
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);
    world.check_committed();
    let reader = world.begin();
    assert!(items(&world, &reader, tree(A)).is_empty());
    let in_b: Vec<Vec<u8>> = items(&world, &reader, tree(B)).into_keys().collect();
    assert_eq!(in_b, [b"b1".to_vec()]);
    assert_eq!(db.private_ids(), 0);
}

/// SC8, the savepoint half of T-SC4: with a scope open, a savepoint is not
/// set, taken or popped. A scope that ends leaves the stack as it found it.
#[test]
fn a_savepoint_is_set_taken_and_popped_only_while_no_scope_is_open() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(407);
    planted(&mut world, &mut rng);
    let (tx, mut set) = (world.begin(), Set::default());
    change(&world, &tx, 1, |items| {
        items.insert(key(900), b"before".to_vec());
    });
    set.set(&tx);
    let saved = grove_hash(&world, &tx);
    change(&world, &tx, 2, |items| {
        items.insert(key(901), b"after".to_vec());
    });

    let scope = tx.txn.scope().expect("no scope is open");
    let before = (tx.txn.stats(), tx.txn.savepoints());
    let refused = Err(SavepointError::ScopesOpen);
    assert_eq!(tx.txn.set_savepoint(), refused);
    assert_eq!(tx.txn.rollback_to_savepoint(), refused);
    assert_eq!(tx.txn.pop_savepoint(), refused);
    assert_eq!(
        SavepointError::ScopesOpen.to_string(),
        "a scope of the transaction is open"
    );
    assert_eq!((tx.txn.stats(), tx.txn.savepoints()), before);
    // The scope writes and is rolled back: its writes go, and the writes
    // after the savepoint that were there before it stay.
    let batch = records(vec![put(aux(), b"scoped", b"rolled back")]);
    batch
        .with(|batch| scope.apply(batch))
        .expect("the scope writes");
    scope.rollback();
    assert_eq!(
        (tx.txn.stats().writes, tx.txn.savepoints()),
        (before.0.writes, 1)
    );
    world.check(&tx);

    // A scope that merges: its writes came after the savepoint, and the
    // rollback to the savepoint takes them out.
    world.write(&tx, vec![put(aux(), b"scoped", b"merged")]);
    assert_eq!(get(&tx, aux(), b"scoped"), Some(b"merged".to_vec()));
    set.roll_back(&world, &tx);
    assert_eq!(get(&tx, aux(), b"scoped"), None);
    assert_eq!(grove_hash(&world, &tx), saved);
    world.check(&tx);

    // A commit with a scope open is refused, and the savepoints stay.
    set.set(&tx);
    let scope = tx.txn.scope().expect("no scope is open");
    let error = db.commit(&versions, &tx.txn).expect_err("a scope is open");
    assert!(matches!(error, TxnError::ScopeOpen), "{error}");
    scope.merge();
    assert_eq!(tx.txn.savepoints(), 1);
    set.roll_back(&world, &tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

/// WT9 and the last line of WT8: a rollback of the transaction removes its
/// savepoints, and a commit ignores the ones that are set, whatever its
/// outcome. Dropping a transaction with savepoints is a rollback.
#[test]
fn a_rollback_removes_the_savepoints_and_a_commit_ignores_them() {
    let (db, versions) = Db::new(48);
    let mut world = World::new(&db, &versions);

    // A rollback: the savepoints go, also the one of the empty write set,
    // which would be a place of every later write set.
    let (tx, mut set) = (world.begin(), Set::default());
    set.set(&tx);
    world.write(&tx, vec![put(aux(), b"k", b"1")]);
    set.set(&tx);
    world.write(&tx, vec![put(aux(), b"k", b"2")]);
    tx.txn.rollback().expect("no scope is open");
    tx.forget();
    assert_eq!(tx.txn.savepoints(), 0);
    world.write(&tx, vec![put(aux(), b"k2", b"written fresh")]);
    assert_eq!(
        tx.txn.rollback_to_savepoint(),
        Err(SavepointError::NoSavepoint)
    );
    assert_eq!(get(&tx, aux(), b"k2"), Some(b"written fresh".to_vec()));
    world.check(&tx);

    // A commit that publishes: the savepoints do not hold it back, and the
    // transaction has none afterwards.
    let mut set = Set::default();
    set.set(&tx);
    world.write(&tx, vec![put(aux(), b"k3", b"3")]);
    set.set(&tx);
    let outcome = db.commit(&versions, &tx.txn).expect("a commit");
    assert!(committed(outcome));
    let writes: Vec<Write> = (tx.model.borrow().writes.iter())
        .map(|(write, ..)| write.clone())
        .collect();
    world.model.commit(&writes);
    tx.forget();
    assert_eq!((tx.txn.savepoints(), tx.txn.stats().writes), (0, 0));
    world.check_committed();
    // What it writes after its commit is not undone by a savepoint from
    // before it.
    world.write(&tx, vec![put(aux(), b"k4", b"4")]);
    assert_eq!(
        tx.txn.rollback_to_savepoint(),
        Err(SavepointError::NoSavepoint)
    );
    world.check(&tx);

    // A commit that fails with a conflict ends the transaction and its
    // savepoints.
    tx.txn.set_savepoint().expect("no scope is open");
    world.write_outside(&records(vec![put(aux(), b"k4", b"w")]), &[&tx]);
    let outcome = db.commit(&versions, &tx.txn).expect("a commit");
    assert_eq!(outcome, Outcome::Conflict);
    tx.forget();
    assert_eq!((tx.txn.savepoints(), tx.txn.stats().writes), (0, 0));

    // A commit of nothing: the savepoints of the empty write set go too.
    tx.txn.set_savepoint().expect("no scope is open");
    tx.txn.set_savepoint().expect("no scope is open");
    let outcome = db.commit(&versions, &tx.txn).expect("a commit");
    assert_eq!(outcome, Outcome::Empty);
    assert_eq!(tx.txn.savepoints(), 0);
    world.write(&tx, vec![put(aux(), b"k5", b"5")]);
    assert_eq!(
        tx.txn.rollback_to_savepoint(),
        Err(SavepointError::NoSavepoint)
    );
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();

    // A commit that fails with an error leaves the transaction as it was,
    // and its savepoints with it. Here the write set cannot be laid out:
    // the transaction made the first node of an empty Merk, and so did a
    // commit outside it, with no record in common. The rollback to the
    // savepoint takes that write out, and the commit goes through.
    fn first_node<'s>(world: &World<'s>, tx: &Tx<'s>, name: &[u8]) {
        let merks = vec![world.merk(tx, 5, |items| {
            items.insert(name.to_vec(), b"a first node".to_vec());
        })];
        let batch = OwnedBatch {
            merks,
            ..OwnedBatch::default()
        };
        world.apply(tx, &batch).expect("a first node");
    }
    let (tx, mut set) = (world.begin(), Set::default());
    world.write(&tx, vec![put(aux(), b"k6", b"6")]);
    set.set(&tx);
    first_node(&world, &tx, b"two");
    set.set(&tx);
    let other = world.begin();
    first_node(&world, &other, b"one");
    assert!(committed(world.commit(other, &[&tx])));
    let held = tx.txn.stats().writes;
    let refused = db.commit(&versions, &tx.txn).expect_err("two roots");
    assert!(
        matches!(refused, TxnError::Batch(BatchError::RootTaken { .. })),
        "{refused}"
    );
    assert_eq!((tx.txn.savepoints(), tx.txn.stats().writes), (2, held));
    world.check(&tx);
    set.roll_back(&world, &tx);
    assert_eq!(tx.txn.stats().writes, held);
    set.roll_back(&world, &tx);
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    let reader = world.begin();
    assert_eq!(get(&reader, aux(), b"k6"), Some(b"6".to_vec()));
    assert_eq!(shown(&reader, model::subtree(5)), [b"one".to_vec()].into());

    // A drop with savepoints set gives everything back.
    let tx = world.begin();
    tx.txn.set_savepoint().expect("no scope is open");
    world.edit(&tx, 1, |items| {
        for node in 0..60 {
            items.insert(key(node), vec![7; 90]);
        }
    });
    tx.txn.set_savepoint().expect("no scope is open");
    assert!(db.private_ids() > 0);
    drop(tx);
    assert_eq!(db.private_ids(), 0);
    assert_eq!(db.pool.census().private, 0);
    world.check_committed();
}

/// ID1, `page-alloc.md` N5: the page ids that the writes after a savepoint
/// took go back to the allocator with the rollback, at once, and the
/// transaction keeps none it does not use.
#[test]
fn the_page_ids_of_rolled_back_writes_go_back_at_once() {
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(408);
    planted(&mut world, &mut rng);
    let ids = || db.id_states().values().sum::<usize>();

    let (tx, mut set) = (world.begin(), Set::default());
    change(&world, &tx, 5, |items| {
        for node in 0..30 {
            items.insert(key(700 + node), value(&mut rng, 100));
        }
    });
    world.check(&tx);
    let held = (db.private_ids(), tx.txn.stats());
    let mut most = 0;
    for round in 0..8 {
        set.set(&tx);
        change(&world, &tx, 6, |items| {
            for node in 0..300 {
                let len = 40 + rng.below(160);
                items.insert(key(3_000 + round * 300 + node), value(&mut rng, len));
            }
        });
        let grown = db.private_ids();
        assert!(grown > held.0 + 6, "{grown} ids are held");
        set.roll_back_unread(&tx);
        // The frames went with the rollback, and their ids with them.
        assert_eq!(db.private_ids(), 0, "round {round}");
        assert_eq!(db.pool.census().private, 0);
        // The next operation makes the frames of the writes that are left.
        world.check(&tx);
        let stats = tx.txn.stats();
        assert_eq!(db.private_ids(), held.0, "round {round}");
        assert_eq!(
            (stats.writes, stats.tree_pages, stats.flat_pages),
            (held.1.writes, held.1.tree_pages, held.1.flat_pages)
        );
        // The ids that went back are handed out again: the allocator knows
        // no more ids after the eighth round than after the first.
        match round {
            0 => most = ids(),
            _ => assert_eq!(ids(), most, "round {round}"),
        }
    }
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
}

type Kept = Vec<(Vec<u8>, NodeHandle)>;

/// Reads every node of the Merk `name` by its key and keeps its handle.
fn keep(world: &World<'_>, tx: &Tx<'_>, name: u8) -> Kept {
    (items(world, tx, name).into_keys())
        .map(|key| {
            let read = |view: Option<NodeView<'_>>| {
                let view = view.expect("the node is there");
                view.handle().expect("a cell has a handle")
            };
            let handle = (tx.txn.node(model::subtree(name), &key, read)).expect("a read");
            (key, handle)
        })
        .collect()
}

/// Reads every kept node through its handle and returns how many of the
/// handles were followed, and the keys under which a node was shown.
fn follow(tx: &Tx<'_>, name: u8, kept: &Kept) -> (u64, BTreeSet<Vec<u8>>) {
    let before = tx.txn.stats().handles_followed;
    let mut shown = BTreeSet::new();
    for (key, handle) in kept {
        let read = |view: Option<NodeView<'_>>| view.map(|view| view.key().to_vec());
        let found = (tx.txn.follow(model::subtree(name), *handle, key, read)).expect("a read");
        assert!(found.as_ref().is_none_or(|found| found == key));
        shown.extend(found);
    }
    (tx.txn.stats().handles_followed - before, shown)
}

/// Keeps the handles of the tree `index`, lets `leave` drop or clear the
/// tree after a savepoint, and rolls back to it.
fn across<'s>(world: &World<'s>, tx: &Tx<'s>, set: &mut Set, index: usize, leave: impl FnOnce()) {
    let name = tree(index);
    let nodes: BTreeSet<Vec<u8>> = items(world, tx, name).into_keys().collect();
    let kept = keep(world, tx, name);
    assert_eq!(follow(tx, name, &kept), (kept.len() as u64, nodes.clone()));
    set.set(tx);
    leave();
    assert_eq!(follow(tx, name, &kept), (0, BTreeSet::new()));
    set.roll_back(world, tx);
    assert_eq!(follow(tx, name, &kept), (0, nodes));
}

/// T-RD6 for a savepoint, and the rule of TREE-02 that every rollback takes
/// a new lease: a handle kept across a rollback to a savepoint is not
/// followed, and the read gives the node the read rule names, by its key.
#[test]
fn a_node_handle_ends_with_a_rollback_to_a_savepoint() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(409);
    planted(&mut world, &mut rng);
    let name = tree(8);
    let (tx, mut set) = (world.begin(), Set::default());
    change(&world, &tx, 8, |items| {
        items.insert(key(800), b"a write, so that there are frames".to_vec());
    });
    let kept = keep(&world, &tx, name);
    let nodes: BTreeSet<Vec<u8>> = items(&world, &tx, name).into_keys().collect();
    assert_eq!(follow(&tx, name, &kept), (kept.len() as u64, nodes.clone()));

    // A rollback to a savepoint that nothing was written after puts no
    // frame back: the handles stay good.
    set.set(&tx);
    set.roll_back(&world, &tx);
    assert_eq!(follow(&tx, name, &kept).0, kept.len() as u64);

    // Writes after the savepoint carry cells and take nodes out. The
    // rollback puts the frames back, and they could be changed again into
    // pages on which an old handle names another node.
    set.set(&tx);
    change(&world, &tx, 8, |items| {
        for key in nodes.iter().step_by(2) {
            items.remove(key);
        }
        for node in 0..150 {
            items.insert(key(4_000 + node), value(&mut rng, 120));
        }
    });
    let later = keep(&world, &tx, name);
    set.roll_back(&world, &tx);
    assert_eq!(
        follow(&tx, name, &kept),
        (0, nodes.clone()),
        "no handle from before the rollback is followed, and every node is found"
    );
    // A handle of a node that the rollback took out finds no node.
    let (followed, shown) = follow(&tx, name, &later);
    assert_eq!(followed, 0);
    assert_eq!(shown, &nodes - &nodes.iter().step_by(2).cloned().collect());
    // Handles that are read now are good again.
    let fresh = keep(&world, &tx, name);
    assert_eq!(follow(&tx, name, &fresh).0, fresh.len() as u64);
    world.check(&tx);

    // A drop and a clear end the handles themselves. The rollback across
    // one gives the nodes back and not the handles: each node is found by
    // its key.
    across(&world, &tx, &mut set, OUTER, || drop_outer(&world, &tx));
    across(&world, &tx, &mut set, 5, || clear(&world, &tx, 5));
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

const STALE: Stale = Stale::of(HashField::Value).with(HashField::KeyValue);

/// The stale hashes of the nodes of one Merk, by key: what a test expects
/// the transaction to show.
type Marks = BTreeMap<Vec<u8>, Stale>;

/// Writes the nodes `keys` of the Merk 1 through `tx`, each with a value
/// hash and a key-value hash that the writer leaves stale, and whatever
/// `edit` changes beside them. Every other node the change set writes, the
/// ones whose links changed among them, has every hash made, and `marks`
/// is told.
fn write_stale<'s>(
    world: &World<'s>,
    tx: &Tx<'s>,
    marks: &mut Marks,
    keys: &[Vec<u8>],
    edit: impl FnOnce(&mut Items),
) {
    let mut merk = world.merk(tx, 1, |items| {
        for key in keys {
            items.insert(key.clone(), b"a value with no hash yet".to_vec());
        }
        edit(items);
    });
    merk.stale = keys.iter().map(|key| (key.clone(), STALE)).collect();
    for (key, ..) in &merk.nodes {
        let stale = merk.stale.get(key).copied().unwrap_or(Stale::NONE);
        marks.insert(key.clone(), stale);
    }
    for key in &merk.removed {
        marks.remove(key);
    }
    let batch = OwnedBatch {
        records: World::root_record(&merk).into_iter().collect(),
        merks: vec![merk],
        ..OwnedBatch::default()
    };
    world.apply(tx, &batch).expect("nodes with stale hashes");
}

/// Checks that `tx` shows the stale hashes of `marks` and no other, for
/// every node of the Merk 1 that it wrote, and returns how many nodes have
/// one.
fn assert_stale(world: &World<'_>, tx: &Tx<'_>, marks: &Marks) -> usize {
    for key in items(world, tx, 1).into_keys() {
        let read = |view: Option<NodeView<'_>>| {
            let view = view.expect("the node is there");
            assert_eq!(view.stale().is_empty(), *view.value_hash() != UNMADE);
            view.stale()
        };
        let shown = (tx.txn.node(model::subtree(1), &key, read)).expect("a read");
        let expected = marks.get(&key).copied().unwrap_or(Stale::NONE);
        assert_eq!(shown, expected, "{key:?}");
    }
    let stale = marks.values().filter(|stale| !stale.is_empty()).count();
    assert_eq!(tx.txn.stats().stale_nodes, stale);
    stale
}

/// No copy of the stale marks is kept at a savepoint: they are a part of
/// the frames, and a rollback makes them again from the writes that are
/// left. Marks that were set before the savepoint and taken off after it
/// are set again, marks that were set after it are gone with their nodes,
/// and the commit makes exactly the hashes that are stale then.
#[test]
fn a_rollback_to_a_savepoint_gives_back_the_stale_hashes_it_had() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(412);
    world.edit_outside(
        1,
        |items| {
            for node in 0..30 {
                items.insert(key(node), value(&mut rng, 60));
            }
        },
        &[],
    );
    let keys = |from: usize| -> Vec<Vec<u8>> { (from..from + 12).map(key).collect() };
    let (first, second, third) = (keys(100), keys(200), keys(300));

    let (tx, mut set) = (world.begin(), Set::default());
    let mut marks = Marks::new();
    write_stale(&world, &tx, &mut marks, &first, |_| ());
    assert_eq!(assert_stale(&world, &tx, &marks), 12);
    let saved = marks.clone();
    set.set(&tx);

    // After the savepoint more nodes are written with stale hashes, some of
    // the first are written again with every hash made, and pages are cut,
    // so marks are carried with their cells.
    write_stale(&world, &tx, &mut marks, &second, |items| {
        for node in 0..60 {
            items.insert(key(1_000 + node), value(&mut rng, 150));
        }
        for key in [&first[0], &first[5]] {
            items.insert(key.clone(), b"another value, with its hashes".to_vec());
        }
    });
    let stale = assert_stale(&world, &tx, &marks);
    assert!((14..=22).contains(&stale), "{stale}");
    let outer = marks.clone();

    // A savepoint inside: the rollback to it sets the marks its writes
    // took off, and takes off the marks they set.
    set.set(&tx);
    write_stale(&world, &tx, &mut marks, &third, |items| {
        items.insert(
            second[3].clone(),
            b"made after the inner savepoint".to_vec(),
        );
        items.remove(&first[7]);
    });
    assert_ne!(assert_stale(&world, &tx, &marks), stale);
    set.roll_back(&world, &tx);
    assert_eq!(assert_stale(&world, &tx, &outer), stale);

    set.roll_back(&world, &tx);
    assert_eq!(assert_stale(&world, &tx, &saved), 12);

    // The commit makes the hashes that are stale after the rollbacks, and
    // no other: a value hash and a key-value hash for each of the twelve.
    let count = |counter: &std::sync::atomic::AtomicU64| counter.load(Ordering::Relaxed);
    assert_eq!(count(&db.counts.rule_calls), 0);
    assert!(committed(world.commit(tx, &[])));
    assert_eq!(
        (count(&db.counts.stale_nodes), count(&db.counts.rule_calls)),
        (12, 24)
    );
    world.check_committed();
    assert_eq!(assert_stale(&world, &world.begin(), &Marks::new()), 0);
    assert_eq!(db.private_ids(), 0);
}

/// A savepoint whose writes went from under it is a fault of the
/// transaction's own code, which the savepoints are there to rule out: a
/// rollback and a commit clear the stack. It is not passed over.
#[test]
#[should_panic(expected = "a savepoint is a place of the write set")]
fn a_savepoint_that_is_no_place_of_the_write_set_is_not_passed_over() {
    let (db, versions) = Db::new(32);
    let world = World::new(&db, &versions);
    let tx = world.begin();
    let empty = tx.txn.mark();
    world.write(&tx, vec![put(aux(), b"a", b"1")]);
    tx.txn.set_savepoint().expect("no scope is open");
    // What no caller outside the crate can do: go back behind a savepoint
    // by a mark.
    assert!(tx.txn.rewind(empty));
    world.write(&tx, vec![put(aux(), b"b", b"2")]);
    let _ = tx.txn.rollback_to_savepoint();
}

/// A transaction of a schedule, with its savepoints as the model has them
/// and the root hashes each had.
struct Open<'s> {
    tx: Tx<'s>,
    set: Set,
    /// For each savepoint: the latest version and the root hashes of the
    /// Merks when it was set, if the transaction's records were a tree
    /// then.
    saved: Vec<(CommitVersion, Option<Vec<Hash>>)>,
}

const MERKS: [u8; 3] = [1, 2, 3];

/// The root hash of every Merk as `tx` reads it. `None` while a commit
/// outside has met a write of `tx`: its records need not fit together then
/// (WT7b), and no hash stands for them.
fn hashes(world: &World<'_>, tx: &Tx<'_>) -> Option<Vec<Hash>> {
    (tx.model.borrow().met() == 0).then(|| MERKS.map(|name| root_hash(world, tx, name)).to_vec())
}

/// A batch of a few changes, as `tx` would make them now.
fn random_batch<'s>(world: &World<'s>, tx: &Tx<'s>, rng: &mut Rng, keys: usize) -> OwnedBatch {
    let mut batch = OwnedBatch::default();
    let key = |rng: &mut Rng| format!("k{:03}", rng.below(keys)).into_bytes();
    if rng.chance(75) {
        let name = MERKS[rng.below(MERKS.len())];
        let merk = world.merk(tx, name, |items| {
            for _ in 0..1 + rng.below(5) {
                let key = key(rng);
                if rng.below(10) < 3 {
                    items.remove(&key);
                } else {
                    let len = match rng.chance(12) {
                        true => LONG + rng.below(300),
                        false => rng.below(180),
                    };
                    items.insert(key, value(rng, len));
                }
            }
        });
        if changes(&merk) {
            batch.records.extend(World::root_record(&merk));
            batch.merks.push(merk);
        }
    }
    if batch.merks.is_empty() || rng.chance(35) {
        let unit = [aux(), meta()][rng.below(2)];
        let key = key(rng);
        batch.records.push(match rng.below(10) {
            0..=5 => {
                let len = rng.below(100);
                put(unit, &key, &value(rng, len))
            }
            6..=8 => delete(unit, &key),
            _ => {
                let mut end = key.clone();
                end.push(b'5');
                Write::Range(unit, key, Some(end))
            }
        });
    }
    batch
}

/// A random schedule of writes, savepoints, rollbacks to them, pops,
/// rollbacks, commits and writes with no transaction, over up to three open
/// transactions, against the model: every read after every step, every
/// commit outcome, and after each rollback to a savepoint the root hash of
/// every Merk, which is the one the savepoint had whenever no commit landed
/// in between. One value in eight is out of line, and pending once it is
/// committed.
fn run(seed: u64, steps: usize, keys: usize) -> (usize, usize) {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(seed);
    let mut open: Vec<Open<'_>> = Vec::new();
    let (mut restored, mut conflicts) = (0, 0);
    for step in 0..steps {
        let at = (!open.is_empty()).then(|| rng.below(open.len()));
        match (rng.below(100), at) {
            (0..=7, _) if open.len() < 3 => open.push(Open {
                tx: world.begin(),
                set: Set::default(),
                saved: Vec::new(),
            }),
            (0..=39, Some(at)) => {
                let this = &open[at];
                let batch = random_batch(&world, &this.tx, &mut rng, keys);
                world.apply(&this.tx, &batch).unwrap_or_else(|error| {
                    panic!("seed {seed}, step {step}: {error}");
                });
                world.check(&this.tx);
            }
            (40..=54, Some(at)) => {
                let this = &mut open[at];
                (this.saved).push((world.latest(), hashes(&world, &this.tx)));
                this.set.set(&this.tx);
            }
            (55..=69, Some(at)) => {
                let this = &mut open[at];
                let Some((version, saved)) = this.saved.pop() else {
                    assert_eq!(
                        this.tx.txn.rollback_to_savepoint(),
                        Err(SavepointError::NoSavepoint)
                    );
                    continue;
                };
                this.set.roll_back(&world, &this.tx);
                world.check(&this.tx);
                if version == world.latest() && saved.is_some() {
                    assert_eq!(
                        hashes(&world, &this.tx),
                        saved,
                        "seed {seed}, step {step}: the root hashes of the savepoint"
                    );
                    restored += 1;
                }
            }
            (70..=73, Some(at)) => {
                let this = &mut open[at];
                if this.saved.pop().is_some() {
                    this.set.pop(&this.tx);
                    world.check(&this.tx);
                }
            }
            (74..=77, Some(at)) => {
                let this = &mut open[at];
                this.tx.txn.rollback().expect("no scope is open");
                this.tx.forget();
                (this.set, this.saved) = (Set::default(), Vec::new());
                assert_eq!(this.tx.txn.savepoints(), 0);
                world.check(&this.tx);
            }
            (78..=87, _) => {
                let tx = world.begin();
                let batch = random_batch(&world, &tx, &mut rng, keys);
                (world.apply(&tx, &batch)).expect("a write with no transaction");
                let others: Vec<&Tx<'_>> = open.iter().map(|open| &open.tx).collect();
                world.commit(tx, &others);
                world.check_committed();
            }
            (88..=95, Some(at)) => {
                let done = open.swap_remove(at);
                let others: Vec<&Tx<'_>> = open.iter().map(|open| &open.tx).collect();
                if world.commit(done.tx, &others) == Outcome::Conflict {
                    conflicts += 1;
                }
                world.check_committed();
            }
            _ => {
                for this in &open {
                    world.check(&this.tx);
                }
            }
        }
    }
    drop(open);
    world.check_committed();
    assert_eq!(
        db.private_ids(),
        0,
        "seed {seed}: no id is left with a transaction"
    );
    assert_eq!(db.pool.census().private, 0, "seed {seed}: no copy is left");
    (restored, conflicts)
}

#[test]
fn random_schedules_with_savepoints_agree_with_the_model() {
    let steps = if cfg!(miri) { 40 } else { 600 };
    let (mut restored, mut conflicts) = (0, 0);
    for seed in 0..6 {
        let (hashes, failed) = run(seed, steps, [60, 12][seed as usize % 2]);
        restored += hashes;
        conflicts += failed;
    }
    assert!(
        restored > 40 || cfg!(miri),
        "{restored} rollbacks compared root hashes"
    );
    assert!(
        conflicts > 0 || cfg!(miri),
        "no commit failed with a conflict"
    );
}
