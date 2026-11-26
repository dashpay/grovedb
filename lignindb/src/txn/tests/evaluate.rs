//! An evaluation before the commit ([`Scope::evaluate`],
//! [`WriteTxn::evaluate`]; `crate::txn`, "An evaluation before the
//! commit"): the root hash it gives, what a reader finds afterwards, what
//! the commit has left to make, and what becomes of the hashes it made
//! when the frames are made again, a savepoint is rolled back to, a scope
//! is taken out, a subtree leaves or moves, and the transaction ends.
//!
//! The writers of these tests leave every hash of every node they write
//! to the engine, and state bytes that are no hash for each
//! ([`model::UNMADE`]). The model has the hashes that the rules make
//! ([`Rules`]), so a node that the model's check passes has them all.

use std::sync::atomic::Ordering;

use lignin_format::tree_page::{Hash, HashField, Side};
use lignin_tree::{
    hash::{HashError, HashRules, Refusal, Stale},
    node::NodeRef,
};

use super::{
    model::{
        self, handle_of, hash_of, value, Committed, OwnedBatch, OwnedMerk, OwnedSubtree, UNMADE,
    },
    store::{Db, Outcome, Rng, Rules, ENTERED},
    *,
};
use crate::txn::{pages::held, FlatKinds};

/// The keys and values of one Merk.
type Items = BTreeMap<Vec<u8>, Vec<u8>>;

/// The keys of the nodes that have a stale hash, by the name of their
/// Merk.
type StaleKeys = BTreeMap<u8, BTreeSet<Vec<u8>>>;

fn sub(name: u8) -> Subtree {
    model::subtree(name)
}

fn committed(outcome: Outcome) -> bool {
    matches!(outcome, Outcome::Committed(_))
}

fn key(index: usize) -> Vec<u8> {
    format!("n{index:04}").into_bytes()
}

/// Leaves every hash of every node that `merk` writes to the engine: its
/// value hash, its key-value hash, and the hash of each child it has.
fn leave(merk: &mut OwnedMerk) {
    for (key, node, _) in &merk.nodes {
        let links = Side::BOTH
            .into_iter()
            .filter(|side| node.link(*side).is_some());
        let stale = links.fold(
            Stale::of(HashField::Value).with(HashField::KeyValue),
            |stale, side| stale.with(HashField::Child(side)),
        );
        merk.stale.insert(key.clone(), stale);
    }
}

/// How many times the rules are called to make the hashes that `merk`
/// leaves, where every node it writes is reached from the root: a value
/// hash and a key-value hash for each node, and the hash of the child
/// under each of its links.
fn calls_for(merk: &OwnedMerk) -> u64 {
    let links = |node: &model::NodeRec| Side::BOTH.map(|side| node.link(side).is_some());
    let made = (merk.nodes.iter())
        .map(|(_, node, _)| 2 + links(node).into_iter().filter(|link| *link).count());
    made.sum::<usize>() as u64
}

/// The change set of the Merk `name` that `edit` makes, with every hash
/// left to the engine.
fn left<'s>(world: &World<'s>, tx: &Tx<'s>, name: u8, edit: impl FnOnce(&mut Items)) -> OwnedMerk {
    let mut merk = world.merk(tx, name, edit);
    leave(&mut merk);
    merk
}

/// The batch of `merks`, each with the record of its root key.
fn batch(merks: Vec<OwnedMerk>) -> OwnedBatch {
    OwnedBatch {
        records: merks.iter().filter_map(World::root_record).collect(),
        merks,
        ..OwnedBatch::default()
    }
}

/// The items of the Merk `name` as `tx` reads them, by the model.
fn items(world: &World<'_>, tx: &Tx<'_>, name: u8) -> Items {
    let unit = Unit::data(sub(name));
    Committed::items(&tx.model.borrow().view(&world.model.records, unit))
}

/// The root hash of the Merk that holds `items`, as the model hashes it.
fn hash_of_items(items: &Items) -> Option<Hash> {
    let none = [0; 32];
    let (root, records) = model::records(items);
    let node = &records[&root?];
    let child = |side| node.link(side).map_or(&none, |link| &link.hash);
    Some(hash_of(&[
        &node.kv_hash,
        child(Side::Left),
        child(Side::Right),
    ]))
}

/// The root hash of the Merk `name` as the model of `tx` has it.
fn expected(world: &World<'_>, tx: &Tx<'_>, name: u8) -> Option<Hash> {
    hash_of_items(&items(world, tx, name))
}

/// What `tx` shows of the node under `key` of the Merk `name`: its stale
/// hashes, its value hash and its key-value hash as they are stored, and
/// whether it is shown from the write set.
fn shown(tx: &Tx<'_>, name: u8, key: &[u8]) -> (Stale, Hash, Hash, bool) {
    let read = |view: Option<NodeView<'_>>| {
        let view = view.unwrap_or_else(|| panic!("no node under {key:?} of {name}"));
        let hashes = (*view.value_hash(), *view.kv_hash());
        (view.stale(), hashes.0, hashes.1, view.is_record())
    };
    (tx.txn.node(sub(name), key, read)).expect("a read")
}

/// What `tx` holds, without the counts of its reads.
fn held_by(tx: &Tx<'_>) -> TxnStats {
    TxnStats {
        handles_followed: 0,
        places_followed: 0,
        keyed_reads: 0,
        ..tx.txn.stats()
    }
}

/// The keys of the nodes of the Merk `name` that `tx` shows with a stale
/// hash.
fn stale_in(world: &World<'_>, tx: &Tx<'_>, name: u8) -> BTreeSet<Vec<u8>> {
    let keys = items(world, tx, name).into_keys();
    keys.filter(|key| !shown(tx, name, key).0.is_empty())
        .collect()
}

/// The keys of the nodes of each Merk of `reached` that `scope` shows
/// with a stale hash.
fn stale_through(
    world: &World<'_>,
    tx: &Tx<'_>,
    scope: &Scope<'_, '_>,
    reached: &[u8],
) -> Vec<(u8, BTreeSet<Vec<u8>>)> {
    let stale_of = |name: &u8| {
        let keys = items(world, tx, *name).into_keys();
        let stale = |key: &Vec<u8>| {
            let read = scope.node(sub(*name), key, |view| view.map(|view| view.stale()));
            read.expect("a read").is_some_and(|stale| !stale.is_empty())
        };
        (*name, keys.filter(stale).collect())
    };
    reached.iter().map(stale_of).collect()
}

/// Tells the model what a call for the Merk `name` writes: every node of
/// `keys`, which had a stale hash, with the record the model has of it.
fn tell<'s>(
    world: &World<'s>,
    tx: &Tx<'s>,
    name: u8,
    keys: Option<&BTreeSet<Vec<u8>>>,
    first_write: u64,
) {
    let Some(keys) = keys.filter(|keys| !keys.is_empty()) else {
        return;
    };
    let unit = Unit::data(sub(name));
    let view = tx.model.borrow().view(&world.model.records, unit);
    let node = |key: &Vec<u8>| match &view[key] {
        Val::Node(node) => (key.clone(), node.clone(), false),
        Val::Flat(_) => panic!("a node has a stale hash"),
    };
    let merk = OwnedMerk {
        nodes: keys.iter().map(node).collect(),
        ..OwnedMerk::change(sub(name), &view, &Committed::items(&view))
    };
    let made = OwnedBatch {
        merks: vec![merk],
        ..OwnedBatch::default()
    };
    world.note(tx, &made, first_write);
}

/// Makes the stale hashes of the Merk `name` through `scope` with
/// `rules`, and tells the model what the call writes: every node of the
/// Merks `reached` that had a stale hash.
fn evaluate_with<'s>(
    world: &World<'s>,
    tx: &Tx<'s>,
    scope: &Scope<'_, 's>,
    name: u8,
    reached: &[u8],
    rules: &mut impl HashRules,
) -> Result<Option<Hash>, TxnError> {
    let stale = stale_through(world, tx, scope, reached);
    let first_write = world.latest().get();
    let hash = scope.evaluate(sub(name), rules)?;
    for (name, keys) in &stale {
        tell(world, tx, *name, Some(keys), first_write);
    }
    Ok(hash)
}

/// [`evaluate_with`] the rules of these tests.
fn evaluate<'s>(
    world: &World<'s>,
    tx: &Tx<'s>,
    scope: &Scope<'_, 's>,
    name: u8,
    reached: &[u8],
) -> Result<Option<Hash>, TxnError> {
    let mut rules = Rules(&world.db.counts.rule_calls);
    evaluate_with(world, tx, scope, name, reached, &mut rules)
}

/// [`evaluate`] through a scope of its own, which merges.
fn evaluate_merged<'s>(world: &World<'s>, tx: &Tx<'s>, name: u8) -> Option<Hash> {
    let scope = tx.txn.scope().expect("a scope");
    let hash = evaluate(world, tx, &scope, name, &[name]).expect("the hashes are made");
    scope.merge();
    hash
}

/// A store whose Merk 1 has forty committed nodes.
fn planted(world: &mut World<'_>, rng: &mut Rng) {
    let made = world.edit_outside(
        1,
        |items| {
            for index in 0..40 {
                items.insert(key(index), value(rng, 60));
            }
        },
        &[],
    );
    assert!(committed(made));
}

/// A change of the Merk `name` that adds nodes, gives some another value
/// and removes one, with every hash left to the engine. `round` tells one
/// such change from another.
fn changed<'s>(world: &World<'s>, tx: &Tx<'s>, rng: &mut Rng, name: u8, round: usize) -> OwnedMerk {
    left(world, tx, name, |items| {
        for index in 0..6 {
            items.insert(key(100 * (round + 1) + index), value(rng, 90));
        }
        for index in [3, 17] {
            items.insert(key(index), value(rng, 70 + round));
        }
        items.remove(&key(20 + round));
    })
}

/// Done when, lines 1 to 4: the root hash is the one the commit
/// publishes, a node that is read afterwards has its hashes, a second
/// call changes nothing, and the commit makes no hash again.
#[test]
fn the_call_gives_the_root_hash_that_the_commit_publishes() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(111);
    planted(&mut world, &mut rng);
    let calls = || db.counts.rule_calls.load(Ordering::Relaxed);
    let evaluated = || db.counts.stale_nodes.load(Ordering::Relaxed);

    let tx = world.begin();
    let merk = changed(&world, &tx, &mut rng, 1, 0);
    let (written, to_make) = (merk.nodes.len(), calls_for(&merk));
    let keys: Vec<Vec<u8>> = merk.nodes.iter().map(|node| node.0.clone()).collect();
    world.apply(&tx, &batch(vec![merk])).expect("the change");
    assert!(written > 8, "{written} nodes are written");
    assert_eq!(tx.txn.stats().stale_nodes, written);
    for key in &keys {
        let (stale, value_hash, kv_hash, _) = shown(&tx, 1, key);
        assert!(stale.has(HashField::Value) && stale.has(HashField::KeyValue));
        assert_eq!((value_hash, kv_hash), (UNMADE, UNMADE), "{key:?}");
    }
    // The handles of the nodes, from before the hashes are made.
    let handles: Vec<_> = (keys.iter())
        .map(|key| (tx.txn.node(sub(1), key, handle_of)).expect("a read"))
        .collect();

    // The call makes each stale hash once, and one hash more: the root's
    // own, which no cell stores.
    let root_hash = expected(&world, &tx, 1);
    let (before, writes) = (calls(), tx.txn.stats().writes);
    held::compared();
    let hash = evaluate_merged(&world, &tx, 1);
    assert_eq!(hash, root_hash);
    assert!(hash.is_some());
    assert_eq!(calls() - before, to_make + 1);
    // Each node it made a hash of is kept under its key, which leads to
    // its cell from the root.
    assert_eq!(held::compared().evaluated, written as u64);
    let stats = tx.txn.stats();
    assert_eq!((stats.stale_nodes, stats.writes), (0, writes + written));

    // A node that is read afterwards has its hashes: by its key, through
    // a handle from before the call, which is good still, and under a
    // cursor.
    world.check(&tx);
    let followed = tx.txn.stats().handles_followed;
    for (key, handle) in keys.iter().zip(&handles) {
        let handle = handle.expect("a cell");
        let read = |view: Option<NodeView<'_>>| {
            let view = view.expect("a node");
            (view.stale(), *view.value_hash(), view.handle())
        };
        let (stale, value_hash, now) = (tx.txn.follow(sub(1), handle, key, read)).expect("a read");
        assert_eq!(stale, Stale::NONE, "{key:?}");
        assert_ne!(value_hash, UNMADE, "{key:?}");
        assert_eq!(now, Some(handle), "{key:?}");
    }
    assert_eq!(
        tx.txn.stats().handles_followed - followed,
        written as u64,
        "no handle ended"
    );

    // A second call makes the root's own hash and nothing else, and adds
    // nothing: through a scope, and through the transaction.
    let stats = held_by(&tx);
    for through_scope in [true, false] {
        let before = calls();
        let again = match through_scope {
            true => evaluate_merged(&world, &tx, 1),
            false => (tx.txn.evaluate(sub(1), &mut Rules(&db.counts.rule_calls))).expect("a hash"),
        };
        assert_eq!(again, hash);
        assert_eq!(calls() - before, 1);
        assert_eq!(held_by(&tx), stats);
    }
    assert_eq!(held::compared().evaluated, 0);

    // The commit makes no hash: none is left to it.
    let before = (calls(), evaluated());
    assert!(committed(world.commit(tx, &[])));
    assert_eq!((calls(), evaluated()), before);
    world.check_committed();
    // The root hash of the committed Merk is the one the call gave.
    let reader = world.begin();
    let published = reader
        .txn
        .evaluate(sub(1), &mut Rules(&db.counts.rule_calls));
    assert_eq!(published.expect("a hash"), hash);
    assert_eq!(reader.txn.stats(), TxnStats::default());
    assert_eq!(db.private_ids(), 0);
}

/// A transaction that never calls it commits as before: the commit makes
/// every hash that was left, and the root hash is the same one.
#[test]
fn the_commit_alone_publishes_the_same_root_hash() {
    let hashes: Vec<Option<Hash>> = [false, true]
        .into_iter()
        .map(|evaluates| {
            let (db, versions) = Db::new(96);
            let mut world = World::new(&db, &versions);
            let mut rng = Rng::new(112);
            planted(&mut world, &mut rng);
            let tx = world.begin();
            let merk = changed(&world, &tx, &mut rng, 1, 0);
            let written = merk.nodes.len() as u64;
            world.apply(&tx, &batch(vec![merk])).expect("the change");
            if evaluates {
                evaluate_merged(&world, &tx, 1);
            }
            assert!(committed(world.commit(tx, &[])));
            let made = db.counts.stale_nodes.load(Ordering::Relaxed);
            assert_eq!(made, if evaluates { 0 } else { written });
            world.check_committed();
            let reader = world.begin();
            let hash = reader
                .txn
                .evaluate(sub(1), &mut Rules(&db.counts.rule_calls));
            hash.expect("a hash")
        })
        .collect();
    assert!(hashes[0].is_some());
    assert_eq!(hashes[0], hashes[1]);
}

/// What the call answers where no hash is stale: a Merk with no node, a
/// Merk that the transaction has not written, a subtree that is not
/// entered, and a transaction that has written nothing. No page is
/// copied and nothing is added.
#[test]
fn a_merk_with_no_stale_hash_is_read_and_not_written() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(113);
    planted(&mut world, &mut rng);
    let calls = || db.counts.rule_calls.load(Ordering::Relaxed);
    let mut rules = Rules(&db.counts.rule_calls);

    // A transaction that has written nothing: the committed Merk.
    let tx = world.begin();
    let committed_hash = expected(&world, &tx, 1);
    let before = calls();
    assert_eq!(
        tx.txn.evaluate(sub(1), &mut rules).expect("a hash"),
        committed_hash
    );
    assert_eq!(tx.txn.evaluate(sub(2), &mut rules).expect("no node"), None);
    assert_eq!(calls() - before, 1);
    assert_eq!(tx.txn.stats(), TxnStats::default());
    let not_entered = |refused: Result<Option<Hash>, TxnError>, name: u8| match refused {
        Err(TxnError::Batch(BatchError::NotEntered { subtree })) => assert_eq!(subtree, sub(name)),
        other => panic!("a subtree that is not entered: {other:?}"),
    };
    not_entered(tx.txn.evaluate(sub(250), &mut rules), 250);

    // With frames: a Merk the transaction wrote with every hash made, and
    // one it has not written.
    world.edit(&tx, 3, |items| {
        drop(items.insert(key(1), value(&mut rng, 50)))
    });
    let stats = tx.txn.stats();
    let before = calls();
    assert_eq!(
        tx.txn.evaluate(sub(1), &mut rules).expect("a hash"),
        committed_hash
    );
    assert_eq!(
        tx.txn.evaluate(sub(3), &mut rules).expect("a hash"),
        expected(&world, &tx, 3)
    );
    assert_eq!(tx.txn.evaluate(sub(2), &mut rules).expect("no node"), None);
    assert_eq!(calls() - before, 2);
    not_entered(tx.txn.evaluate(sub(250), &mut rules), 250);
    assert_eq!(
        tx.txn.stats(),
        stats,
        "nothing is copied and nothing is added"
    );
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

/// Done when, line 6, the first half: a rollback to a savepoint from
/// before the call takes the made hashes out, and the call then gives
/// the root hash again; a savepoint from after the call keeps them.
#[test]
fn a_rollback_to_a_savepoint_takes_the_made_hashes_out_or_keeps_them() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(114);
    planted(&mut world, &mut rng);
    let calls = || db.counts.rule_calls.load(Ordering::Relaxed);
    let evaluated = || db.counts.stale_nodes.load(Ordering::Relaxed);

    let tx = world.begin();
    let first = changed(&world, &tx, &mut rng, 1, 0);
    let to_make = calls_for(&first);
    world.apply(&tx, &batch(vec![first])).expect("the change");
    let stale = stale_in(&world, &tx, 1);
    let root_hash = expected(&world, &tx, 1);

    // A savepoint, the call, and a rollback past the call: every hash
    // that was stale at the savepoint is stale, with the bytes its
    // writer stated, and no write of the call is left.
    let (stats, model_writes) = (tx.txn.stats(), tx.model.borrow().writes.len());
    tx.txn.set_savepoint().expect("no scope is open");
    assert_eq!(evaluate_merged(&world, &tx, 1), root_hash);
    assert_eq!(stale_in(&world, &tx, 1), BTreeSet::new());
    tx.txn.rollback_to_savepoint().expect("a savepoint is set");
    tx.model.borrow_mut().writes.truncate(model_writes);
    assert_eq!(stale_in(&world, &tx, 1), stale);
    for key in &stale {
        assert_eq!(shown(&tx, 1, key).1, UNMADE, "{key:?}");
    }
    let now = tx.txn.stats();
    assert_eq!(
        (now.writes, now.stale_nodes, now.rebuilds),
        (stats.writes, stale.len(), stats.rebuilds + 1)
    );

    // The same holds from here: the call gives the root hash, the nodes
    // have their hashes, and a second call makes the root's hash alone.
    let before = calls();
    assert_eq!(evaluate_merged(&world, &tx, 1), root_hash);
    assert_eq!(calls() - before, to_make + 1);
    world.check(&tx);
    let before = calls();
    assert_eq!(evaluate_merged(&world, &tx, 1), root_hash);
    assert_eq!(calls() - before, 1);

    // A savepoint from after the call, writes after it that leave other
    // hashes stale, the call again, and a rollback: the hashes that were
    // made before the savepoint are made still, and no hash is stale.
    let model_writes = tx.model.borrow().writes.len();
    tx.txn.set_savepoint().expect("no scope is open");
    let second = changed(&world, &tx, &mut rng, 1, 1);
    world
        .apply(&tx, &batch(vec![second]))
        .expect("another change");
    assert!(!stale_in(&world, &tx, 1).is_empty());
    let later = expected(&world, &tx, 1);
    assert_ne!(later, root_hash);
    assert_eq!(evaluate_merged(&world, &tx, 1), later);
    tx.txn.rollback_to_savepoint().expect("a savepoint is set");
    tx.model.borrow_mut().writes.truncate(model_writes);
    assert_eq!(stale_in(&world, &tx, 1), BTreeSet::new());
    world.check(&tx);
    let before = calls();
    assert_eq!(evaluate_merged(&world, &tx, 1), root_hash);
    assert_eq!(calls() - before, 1, "nothing was left to make");

    // The commit makes no hash.
    let before = (calls(), evaluated());
    assert!(committed(world.commit(tx, &[])));
    assert_eq!((calls(), evaluated()), before);
    world.check_committed();
    let reader = world.begin();
    let published = reader
        .txn
        .evaluate(sub(1), &mut Rules(&db.counts.rule_calls));
    assert_eq!(published.expect("a hash"), root_hash);
}

/// Frames that are made again have the made hashes again: after a commit
/// beside the transaction (WT7a), the nodes have their hashes, the call
/// makes nothing, and the commit makes nothing.
#[test]
fn frames_that_are_made_again_have_the_made_hashes() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(115);
    planted(&mut world, &mut rng);
    let calls = || db.counts.rule_calls.load(Ordering::Relaxed);
    let evaluated = || db.counts.stale_nodes.load(Ordering::Relaxed);

    let tx = world.begin();
    for round in 0..3 {
        let merk = changed(&world, &tx, &mut rng, 1, round);
        world.apply(&tx, &batch(vec![merk])).expect("the change");
        if round == 1 {
            // A call in the middle: the last change leaves hashes again.
            evaluate_merged(&world, &tx, 1);
        }
    }
    let root_hash = expected(&world, &tx, 1);
    assert_eq!(evaluate_merged(&world, &tx, 1), root_hash);
    let rebuilds = tx.txn.stats().rebuilds;
    let before = (calls(), evaluated());
    world.edit_outside(
        2,
        |items| drop(items.insert(key(1), value(&mut rng, 30))),
        &[&tx],
    );
    world.check(&tx);
    let stats = tx.txn.stats();
    assert_eq!((stats.rebuilds, stats.stale_nodes), (rebuilds + 1, 0));
    assert_eq!(stale_in(&world, &tx, 1), BTreeSet::new());
    // The commit beside it made no hash, and neither do the frames.
    assert_eq!((calls(), evaluated()), before);
    assert_eq!(evaluate_merged(&world, &tx, 1), root_hash);
    assert_eq!(calls() - before.0, 1);

    let before = (calls(), evaluated());
    assert!(committed(world.commit(tx, &[])));
    assert_eq!((calls(), evaluated()), before);
    world.check_committed();
}

/// A transaction that a commit has met serves its records by key (WT7b):
/// a node whose hashes were made is shown with them, from the write set,
/// and the call is refused as a conflict.
#[test]
fn a_transaction_that_is_met_shows_the_made_hashes_and_makes_none() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(116);
    planted(&mut world, &mut rng);
    let tx = world.begin();
    let merk = changed(&world, &tx, &mut rng, 1, 0);
    world.apply(&tx, &batch(vec![merk])).expect("the change");
    // One Merk more, whose hashes are not made.
    let other = left(&world, &tx, 4, |items| {
        drop(items.insert(key(1), value(&mut rng, 40)))
    });
    world.apply(&tx, &batch(vec![other])).expect("another Merk");
    let made = stale_in(&world, &tx, 1);
    evaluate_merged(&world, &tx, 1);

    // A commit beside the transaction writes a node that it wrote.
    world.edit_outside(
        1,
        |items| drop(items.insert(key(3), value(&mut rng, 20))),
        &[&tx],
    );
    for key in &made {
        let (stale, value_hash, kv_hash, record) = shown(&tx, 1, key);
        assert!(record, "{key:?} is shown from the write set");
        assert_eq!(stale, Stale::NONE, "{key:?}");
        assert!(value_hash != UNMADE && kv_hash != UNMADE, "{key:?}");
    }
    // The commit beside it met the first write of the node and the write
    // that the call made of it: both are entries of the key.
    let stats = tx.txn.stats();
    assert!(stats.overlapped >= 2 && stats.base.is_none(), "{stats:?}");
    assert_eq!(stats.overlapped, tx.model.borrow().met());
    world.check_unit(&tx, Unit::data(sub(1)));
    let (stale, value_hash, _, record) = shown(&tx, 4, &key(1));
    assert!(record && !stale.is_empty() && value_hash == UNMADE);

    for name in [1, 4, 2] {
        let refused = tx
            .txn
            .evaluate(sub(name), &mut Rules(&db.counts.rule_calls));
        match &refused {
            Err(TxnError::Batch(BatchError::EvaluatedByKey { subtree })) => {
                assert_eq!(*subtree, sub(name))
            }
            other => panic!("the transaction is met: {other:?}"),
        }
        assert!(refused.expect_err("refused").is_conflict());
    }
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);
    assert_eq!(db.private_ids(), 0);
}

/// Done when, line 6, the second half: scopes that are open side by side
/// each make the hashes of their own Merks, a scope is refused a Merk
/// that another holds, and the hashes that a scope made stay made when
/// another scope is rolled back.
#[test]
fn scopes_make_the_hashes_of_their_merks_side_by_side() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(117);
    planted(&mut world, &mut rng);
    let calls = || db.counts.rule_calls.load(Ordering::Relaxed);
    let evaluated = || db.counts.stale_nodes.load(Ordering::Relaxed);
    let overlap = |refused: Result<Option<Hash>, TxnError>, name: u8| match refused {
        Err(TxnError::ScopeOverlap { unit }) => assert_eq!(unit, Unit::data(sub(name))),
        other => panic!("another scope holds the Merk {name}: {other:?}"),
    };

    let tx = world.begin();
    let a = tx.txn.scope().expect("a scope");
    let b = tx.txn.scope().expect("a scope beside it");
    let c = tx.txn.scope().expect("a third");
    // Each writes a Merk of its own, with every hash left.
    for (scope, name) in [(&a, 1), (&b, 5), (&c, 6)] {
        let merk = left(&world, &tx, name, |items| {
            for index in 0..12 {
                items.insert(key(200 + index), value(&mut rng, 60));
            }
        });
        let through = batch(vec![merk]).through(scope);
        world.apply_in(&tx, scope, &through).expect("the change");
    }
    assert!(a.is_laid_out() && !b.is_laid_out() && !c.is_laid_out());
    let hashes = [1, 5, 6].map(|name| expected(&world, &tx, name));

    // A scope is refused the Merk of another, and makes nothing there. A
    // call through the transaction is refused a Merk that a scope writes.
    overlap(b.evaluate(sub(1), &mut Rules(&db.counts.rule_calls)), 1);
    overlap(a.evaluate(sub(5), &mut Rules(&db.counts.rule_calls)), 5);
    overlap(
        tx.txn.evaluate(sub(1), &mut Rules(&db.counts.rule_calls)),
        1,
    );
    assert_eq!(calls(), 0);

    // Each makes the hashes of its own. The scope that kept its writes in
    // the write set has them laid out for it.
    assert_eq!(
        evaluate(&world, &tx, &a, 1, &[1]).expect("a hash"),
        hashes[0]
    );
    assert_eq!(
        evaluate(&world, &tx, &b, 5, &[5]).expect("a hash"),
        hashes[1]
    );
    assert!(b.is_laid_out() && !c.is_laid_out());
    for (scope, name) in [(&a, 1), (&b, 5)] {
        let unit = Unit::data(sub(name));
        for key in items(&world, &tx, name).into_keys() {
            let read = scope.node(unit.subtree, &key, |view| view.map(|view| view.stale()));
            assert_eq!(
                read.expect("a read"),
                Some(Stale::NONE),
                "{key:?} of {name}"
            );
        }
    }

    // The third scope is rolled back with its writes, and a scope whose
    // hashes were made is rolled back: the frames are made again, and
    // the hashes that the first scope made are made still.
    let rebuilds = tx.txn.stats().rebuilds;
    c.rollback();
    b.rollback();
    {
        let mut model = tx.model.borrow_mut();
        model.writes.retain(|(write, ..)| {
            ![5, 6]
                .into_iter()
                .any(|name| write.indexed(Unit::data(sub(name))))
                && ![5, 6]
                    .into_iter()
                    .any(|name| write.indexed(model::unit(Column::Roots, name)))
        });
    }
    let read = a.node(sub(1), &key(200), |view| view.map(|view| view.stale()));
    assert_eq!(read.expect("a read"), Some(Stale::NONE));
    assert_eq!(tx.txn.stats().rebuilds, rebuilds + 1);
    let before = calls();
    assert_eq!(
        evaluate(&world, &tx, &a, 1, &[1]).expect("a hash"),
        hashes[0]
    );
    assert_eq!(calls() - before, 1);
    a.merge();
    world.check(&tx);
    assert_eq!(tx.txn.stats().stale_nodes, 0);

    // A Merk with no stale hash is read through the transaction beside
    // an open scope, as any read through it is.
    let beside = tx.txn.scope().expect("a scope");
    let before = calls();
    let read = tx.txn.evaluate(sub(1), &mut Rules(&db.counts.rule_calls));
    assert_eq!(read.expect("a read"), hashes[0]);
    assert_eq!(calls() - before, 1);
    beside.rollback();

    // A scope makes the hashes that a merged scope left, and is rolled
    // back: they are stale again, as if it had never been opened.
    let merk = changed(&world, &tx, &mut rng, 1, 2);
    world.apply(&tx, &batch(vec![merk])).expect("a change");
    let stale = stale_in(&world, &tx, 1);
    let (stats, model_writes) = (tx.txn.stats(), tx.model.borrow().writes.len());
    let root_hash = expected(&world, &tx, 1);
    let d = tx.txn.scope().expect("a scope");
    // A write goes through a scope: while one is open, a call through
    // the transaction makes no hash, whatever the scope holds.
    let refused = tx.txn.evaluate(sub(1), &mut Rules(&db.counts.rule_calls));
    assert!(matches!(refused, Err(TxnError::ScopeOpen)), "{refused:?}");
    assert_eq!(stale_in(&world, &tx, 1), stale);
    assert_eq!(
        evaluate(&world, &tx, &d, 1, &[1]).expect("a hash"),
        root_hash
    );
    // While the scope is open, a call through the transaction makes no
    // hash: it is refused the Merk the scope writes.
    overlap(
        tx.txn.evaluate(sub(1), &mut Rules(&db.counts.rule_calls)),
        1,
    );
    d.rollback();
    tx.model.borrow_mut().writes.truncate(model_writes);
    assert_eq!(stale_in(&world, &tx, 1), stale);
    let now = tx.txn.stats();
    assert_eq!((now.writes, now.stale_nodes), (stats.writes, stale.len()));

    // Through the transaction, with no scope open, the call makes them.
    let before = calls();
    let made = tx.txn.evaluate(sub(1), &mut Rules(&db.counts.rule_calls));
    assert_eq!(made.expect("a hash"), root_hash);
    assert!(calls() - before > 1);
    assert_eq!(tx.txn.stats().stale_nodes, 0);
    world.check_unit(&tx, Unit::data(sub(1)));
    let before = (calls(), evaluated());
    let published = db.commit(&versions, &tx.txn).expect("a commit");
    assert!(committed(published));
    assert_eq!((calls(), evaluated()), before);
}

/// A Merk with no host, whose elements host the Merks [`UNDER`].
const TOP: u8 = 9;
const UNDER: [u8; 3] = [ENTERED + 31, ENTERED + 32, ENTERED + 33];

/// The key of the element of [`TOP`] that hosts the Merk `UNDER[at]`.
fn host_of(at: usize) -> Vec<u8> {
    format!("host-{at}").into_bytes()
}

/// Commits [`TOP`] with twelve items and three elements, each of which
/// hosts a Merk of five nodes that is entered under it.
fn grove(world: &mut World<'_>) {
    let tx = world.begin();
    let mut top = world.merk(&tx, TOP, |items| {
        // Items on either side of the elements, in the order of the keys.
        for node in 0..6 {
            items.insert(key(node), vec![3; 40]);
            items.insert(format!("a{node}").into_bytes(), vec![4; 30]);
        }
        for at in 0..UNDER.len() {
            items.insert(host_of(at), b"tree".to_vec());
        }
    });
    let mut made = OwnedBatch::default();
    for (at, name) in UNDER.into_iter().enumerate() {
        top.hosted.insert(host_of(at), vec![sub(name)]);
        let nodes = (0..5).map(|node| (key(node * 2), vec![name; 30 + node * 7]));
        (made.merks).push(OwnedMerk::change(
            sub(name),
            &BTreeMap::new(),
            &nodes.collect(),
        ));
        let host = (sub(TOP), host_of(at), 0);
        (made.subtrees).push(OwnedSubtree::Enter(sub(name), Some(host)));
    }
    made.records.extend(World::root_record(&top));
    made.merks.push(top);
    world.apply(&tx, &made).expect("the Merks are entered");
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

/// A batch that changes the Merks `UNDER[at]` of `which` and writes the
/// element of each in [`TOP`], which names its Merk again, with every
/// hash of every node left to the engine.
fn below<'s>(world: &World<'s>, tx: &Tx<'s>, which: &[usize], round: usize) -> OwnedBatch {
    let mut merks = Vec::new();
    for at in which {
        merks.push(left(world, tx, UNDER[*at], |items| {
            items.insert(key(50 + round), vec![round as u8; 60]);
            items.insert(key(2), vec![round as u8; 25]);
        }));
    }
    let mut top = left(world, tx, TOP, |items| {
        for at in which {
            items.insert(host_of(*at), format!("tree, round {round}").into_bytes());
        }
    });
    for at in which {
        top.hosted.insert(host_of(*at), vec![sub(UNDER[*at])]);
    }
    let records = World::root_record(&top).into_iter().collect();
    merks.push(top);
    OwnedBatch {
        merks,
        records,
        ..OwnedBatch::default()
    }
}

/// The rules of these tests, which keep what they are given as the roots
/// of the Merks that a node hosts.
struct Hosting<'a> {
    rules: Rules<'a>,
    hosted: BTreeMap<Vec<u8>, Vec<Option<Hash>>>,
}

impl HashRules for Hosting<'_> {
    fn value_hash(
        &mut self,
        node: NodeRef<'_>,
        value: &[u8],
        hosted: &[Option<Hash>],
    ) -> Result<Hash, Refusal> {
        if !hosted.is_empty() {
            let twice = self.hosted.insert(node.key().to_vec(), hosted.to_vec());
            assert!(twice.is_none(), "a value hash is made twice");
        }
        self.rules.value_hash(node, value, hosted)
    }

    fn kv_hash(&mut self, key: &[u8], value_hash: &Hash) -> Result<Hash, Refusal> {
        self.rules.kv_hash(key, value_hash)
    }

    fn node_hash(&mut self, node: NodeRef<'_>) -> Result<Hash, Refusal> {
        self.rules.node_hash(node)
    }
}

/// The value hash of a host is made from the roots of the Merks it
/// hosts: the call makes the stale hashes of those Merks first, each kept
/// as an operation of its own, and a Merk whose hashes were made before
/// is read.
#[test]
fn the_call_makes_the_hashes_of_the_merks_that_stale_hosts_point_at() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    grove(&mut world);
    let calls = || db.counts.rule_calls.load(Ordering::Relaxed);
    let evaluated = || db.counts.stale_nodes.load(Ordering::Relaxed);

    let tx = world.begin();
    let changed = below(&world, &tx, &[0, 1], 1);
    let written: usize = changed.merks.iter().map(|merk| merk.nodes.len()).sum();
    let to_make: u64 = changed.merks.iter().map(calls_for).sum();
    world.apply(&tx, &changed).expect("the change");
    assert_eq!(tx.txn.stats().stale_nodes, written);
    let hashes = [TOP, UNDER[0], UNDER[1], UNDER[2]].map(|name| expected(&world, &tx, name));
    let writes = tx.txn.stats().writes;

    // One call for the Merk at the top makes them all: the hash of each
    // hosted root is made for the value hash of its host, and is the
    // root hash of that Merk.
    let scope = tx.txn.scope().expect("a scope");
    let mut rules = Hosting {
        rules: Rules(&db.counts.rule_calls),
        hosted: BTreeMap::new(),
    };
    let reached = [TOP, UNDER[0], UNDER[1]];
    let before = calls();
    held::compared();
    let hash = evaluate_with(&world, &tx, &scope, TOP, &reached, &mut rules);
    assert_eq!(hash.expect("the hashes are made"), hashes[0]);
    scope.merge();
    let given: BTreeMap<Vec<u8>, Vec<Option<Hash>>> = [0, 1]
        .into_iter()
        .map(|at| (host_of(at), vec![hashes[1 + at]]))
        .collect();
    assert_eq!(rules.hosted, given);
    // Beside the stale hashes: the hash of each hosted root, and of the
    // root at the top.
    assert_eq!(calls() - before, to_make + 3);
    assert_eq!(held::compared().evaluated, written as u64);
    let stats = tx.txn.stats();
    assert_eq!((stats.stale_nodes, stats.writes), (0, writes + written));
    world.check(&tx);

    // The Merk that no stale host points at was not read, and each of
    // the others answers with the hash that was made of its root.
    for (at, name) in UNDER.into_iter().enumerate() {
        let before = calls();
        assert_eq!(evaluate_merged(&world, &tx, name), hashes[1 + at]);
        assert_eq!(calls() - before, 1);
    }

    // Frames that are made again have the hashes of every one of them.
    let rebuilds = tx.txn.stats().rebuilds;
    world.edit_outside(2, |items| drop(items.insert(key(1), vec![1; 30])), &[&tx]);
    world.check(&tx);
    let stats = tx.txn.stats();
    assert_eq!((stats.rebuilds, stats.stale_nodes), (rebuilds + 1, 0));
    let before = (calls(), evaluated());
    assert!(committed(world.commit(tx, &[])));
    assert_eq!((calls(), evaluated()), before);
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
}

/// An element can host more than one Merk. The call names the Merk that
/// each pointer of the host is of by the place of the pointer, and the
/// value hash is made from the roots in the order of the places.
#[test]
fn a_host_of_two_merks_has_each_named_by_its_place() {
    const HOST: &[u8] = b"host of two";
    const FIRST: u8 = ENTERED + 41;
    const SECOND: u8 = ENTERED + 42;

    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    grove(&mut world);
    let calls = || db.counts.rule_calls.load(Ordering::Relaxed);
    let evaluated = || db.counts.stale_nodes.load(Ordering::Relaxed);
    let two = [sub(FIRST), sub(SECOND)];

    // The two Merks are entered under one element, at two places, and
    // committed with their hashes made.
    let tx = world.begin();
    let mut top = world.merk(&tx, TOP, |items| {
        drop(items.insert(HOST.to_vec(), b"two trees".to_vec()))
    });
    top.hosted.insert(HOST.to_vec(), two.to_vec());
    let mut made = OwnedBatch::default();
    for (place, name) in [FIRST, SECOND].into_iter().enumerate() {
        made.merks.push(world.merk(&tx, name, |items| {
            for node in 0..3 + place {
                items.insert(key(node), vec![name; 30]);
            }
        }));
        let host = (sub(TOP), HOST.to_vec(), place as u8);
        (made.subtrees).push(OwnedSubtree::Enter(sub(name), Some(host)));
    }
    made.records.extend(World::root_record(&top));
    made.merks.push(top);
    world.apply(&tx, &made).expect("the Merks are entered");
    assert!(committed(world.commit(tx, &[])));

    // Both are changed with their hashes left, and so is their host.
    let tx = world.begin();
    let mut merks: Vec<OwnedMerk> = [FIRST, SECOND]
        .into_iter()
        .map(|name| {
            left(&world, &tx, name, |items| {
                drop(items.insert(key(9), vec![name; 44]))
            })
        })
        .collect();
    let mut top = left(&world, &tx, TOP, |items| {
        drop(items.insert(HOST.to_vec(), b"two trees, changed".to_vec()))
    });
    top.hosted.insert(HOST.to_vec(), two.to_vec());
    merks.push(top);
    let written: usize = merks.iter().map(|merk| merk.nodes.len()).sum();
    world.apply(&tx, &batch(merks)).expect("the change");
    let hashes = [TOP, FIRST, SECOND].map(|name| expected(&world, &tx, name));
    assert_ne!(hashes[1], hashes[2]);

    let scope = tx.txn.scope().expect("a scope");
    let mut rules = Hosting {
        rules: Rules(&db.counts.rule_calls),
        hosted: BTreeMap::new(),
    };
    held::compared();
    let reached = [TOP, FIRST, SECOND];
    let hash = evaluate_with(&world, &tx, &scope, TOP, &reached, &mut rules);
    assert_eq!(hash.expect("the hashes are made"), hashes[0]);
    scope.merge();
    assert_eq!(rules.hosted.get(HOST), Some(&vec![hashes[1], hashes[2]]));
    assert_eq!(held::compared().evaluated, written as u64);
    assert_eq!(tx.txn.stats().stale_nodes, 0);
    world.check(&tx);
    for (name, hash) in [(FIRST, hashes[1]), (SECOND, hashes[2])] {
        let before = calls();
        assert_eq!(evaluate_merged(&world, &tx, name), hash);
        assert_eq!(calls() - before, 1);
    }

    // Frames that are made again keep each Merk's hashes under its own
    // prefix, and the commit makes none.
    world.edit_outside(2, |items| drop(items.insert(key(1), vec![1; 30])), &[&tx]);
    world.check(&tx);
    let before = (calls(), evaluated());
    assert!(committed(world.commit(tx, &[])));
    assert_eq!((calls(), evaluated()), before);
    world.check_committed();
}

/// The deepest first, as a finalize step goes: a Merk whose hashes were
/// made is read by the call for the Merk above it, which makes the rest.
#[test]
fn a_merk_whose_hashes_were_made_is_read_for_the_host_above_it() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    grove(&mut world);
    let calls = || db.counts.rule_calls.load(Ordering::Relaxed);
    let tx = world.begin();
    let changed = below(&world, &tx, &[0, 1], 1);
    let to_make: Vec<u64> = changed.merks.iter().map(calls_for).collect();
    world.apply(&tx, &changed).expect("the change");
    let hashes = [TOP, UNDER[0], UNDER[1]].map(|name| expected(&world, &tx, name));

    let before = calls();
    assert_eq!(evaluate_merged(&world, &tx, UNDER[0]), hashes[1]);
    assert_eq!(calls() - before, to_make[0] + 1);
    assert_eq!(stale_in(&world, &tx, UNDER[0]), BTreeSet::new());
    assert!(!stale_in(&world, &tx, UNDER[1]).is_empty());
    assert!(!stale_in(&world, &tx, TOP).is_empty());

    let scope = tx.txn.scope().expect("a scope");
    let before = calls();
    let hash = evaluate(&world, &tx, &scope, TOP, &[TOP, UNDER[1]]);
    assert_eq!(hash.expect("the hashes are made"), hashes[0]);
    scope.merge();
    // The hash of the root of the first Merk is made once more, from its
    // cell, and nothing below that root.
    assert_eq!(calls() - before, to_make[1] + to_make[2] + 3);
    assert_eq!(tx.txn.stats().stale_nodes, 0);
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

/// A host that is marked pending is behind the roots it hosts
/// (`transactions.md` SC9): its value hash is not made, and the call is
/// refused before a hash is made.
#[test]
fn a_host_that_is_marked_pending_refuses_the_call_and_nothing_is_made() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    grove(&mut world);
    let tx = world.begin();
    let changed = below(&world, &tx, &[0, 1], 1);
    world.apply(&tx, &changed).expect("the change");
    let stale: Vec<BTreeSet<Vec<u8>>> = [TOP, UNDER[0], UNDER[1]]
        .into_iter()
        .map(|name| stale_in(&world, &tx, name))
        .collect();
    let root_hash = expected(&world, &tx, TOP);

    // The element that is marked is one that the evaluation of the tree
    // layer comes to after it has stored a hash: on the way to it from
    // the root there is a node with a child on its left, which is made
    // first, and the element on its right. So a call that left the
    // refusal to that evaluation would have changed a page before it.
    let marked = [0, 1].map(host_of).into_iter().find(|host| {
        let (root, nodes) = model::records(&items(&world, &tx, TOP));
        let mut at = root.expect("a root");
        let mut after_another = false;
        while at != *host {
            let node = &nodes[&at];
            let side = match *host < at {
                true => Side::Left,
                false => Side::Right,
            };
            after_another |= side == Side::Right && node.link(Side::Left).is_some();
            at = node.link(side).expect("the element is there").key.clone();
        }
        after_another
    });
    let marked = marked.expect("an element that the evaluation comes to after another node");
    let scope = tx.txn.scope().expect("a scope");
    scope.mark_pending(sub(TOP), &marked).expect("a mark");
    let held = held_by(&tx);
    let refused = scope.evaluate(sub(TOP), &mut Rules(&db.counts.rule_calls));
    match refused {
        Err(TxnError::Hash(HashError::HostBehind { host })) => {
            let cell = (scope.node(sub(TOP), &marked, handle_of)).expect("a read");
            assert_eq!(Some(host), cell.map(|handle| handle.address()));
        }
        other => panic!("the host is behind: {other:?}"),
    }
    assert_eq!(db.counts.rule_calls.load(Ordering::Relaxed), 0);
    assert_eq!(held_by(&tx), held, "no page was changed and no write added");
    for (name, stale) in [TOP, UNDER[0], UNDER[1]].into_iter().zip(&stale) {
        let keys = items(&world, &tx, name).into_keys();
        let now: BTreeSet<Vec<u8>> = keys
            .filter(|key| {
                let read = scope.node(sub(name), key, |view| view.map(|view| view.stale()));
                read.expect("a read").is_some_and(|stale| !stale.is_empty())
            })
            .collect();
        assert_eq!(&now, stale, "the Merk {name}");
    }
    // The Merk under it is not behind anything: its hashes are made.
    let (under, other) = match marked == host_of(1) {
        true => (UNDER[1], UNDER[0]),
        false => (UNDER[0], UNDER[1]),
    };
    let made = evaluate(&world, &tx, &scope, under, &[under]);
    assert_eq!(made.expect("a hash"), expected(&world, &tx, under));

    // With the mark taken off, the call makes the rest.
    scope
        .clear_pending(sub(TOP), &marked)
        .expect("the mark goes");
    let hash = evaluate(&world, &tx, &scope, TOP, &[TOP, other]);
    assert_eq!(hash.expect("the hashes are made"), root_hash);
    scope.merge();
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

/// A write of the Merk `UNDER[0]` through `scope` that stops below its
/// host.
fn write_under<'s>(
    world: &World<'s>,
    tx: &Tx<'s>,
    scope: &Scope<'_, 's>,
) -> Result<Applied, TxnError> {
    let own = left(world, tx, UNDER[0], |items| {
        drop(items.insert(key(70), vec![7; 30]))
    });
    let own = OwnedBatch {
        merks: vec![OwnedMerk {
            anchored: true,
            ..own
        }],
        ..OwnedBatch::default()
    };
    world.apply_in(tx, scope, &own.through(scope))
}

/// The claims of a call are those of the Merks it comes to (SC3): a read
/// of each Merk before anything of it is read, which the scope keeps as
/// it keeps any read, and then a write of each Merk a hash is made in,
/// all of those or none. A write that is refused names the Merk, which
/// the caller has read. A read under a host that is refused is the
/// host's refusal: the caller is told of no Merk it has not read.
#[test]
fn a_call_claims_each_merk_before_it_reads_it() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    grove(&mut world);
    let overlap = |refused: Result<Option<Hash>, TxnError>, name: u8| match refused {
        Err(TxnError::ScopeOverlap { unit }) => assert_eq!(unit, Unit::data(sub(name))),
        other => panic!("another scope holds the Merk {name}: {other:?}"),
    };
    let tx = world.begin();
    let changed = below(&world, &tx, &[0, 1], 1);
    world.apply(&tx, &changed).expect("the change");
    // The hashes of the first Merk are made, so that the call for the
    // top reads it, and makes the hashes of the second.
    evaluate_merged(&world, &tx, UNDER[0]);
    let root_hash = expected(&world, &tx, TOP);

    // A scope has read both Merks. The call reads both with it, and
    // would write the second: it is refused that one, makes no hash and
    // changes nothing.
    let other = tx.txn.scope().expect("a scope");
    let caller = tx.txn.scope().expect("a scope beside it");
    for name in [UNDER[0], UNDER[1]] {
        let read = other.node(sub(name), &key(2), |view| view.is_some());
        assert!(read.expect("a read"));
    }
    let (held, calls) = (held_by(&tx), db.counts.rule_calls.load(Ordering::Relaxed));
    overlap(
        caller.evaluate(sub(TOP), &mut Rules(&db.counts.rule_calls)),
        UNDER[1],
    );
    assert_eq!(db.counts.rule_calls.load(Ordering::Relaxed), calls);
    assert_eq!(held_by(&tx), held);

    // No write claim of the call was kept, and every read it made was:
    // of the Merk that was asked for, and of the Merks it came to. The
    // other scope is refused a write of each.
    let model_writes = tx.model.borrow().writes.len();
    overlap(write_under(&world, &tx, &other).map(|_| None), UNDER[0]);
    let top = left(&world, &tx, TOP, |items| {
        drop(items.insert(key(71), vec![7; 30]))
    });
    let refused = world.apply_in(&tx, &other, &batch(vec![top]).through(&other));
    overlap(refused.map(|_| None), TOP);
    tx.model.borrow_mut().writes.truncate(model_writes);
    other.rollback();
    caller.rollback();

    // A scope that keeps its writes in the write set writes the Merk
    // that the call reads under a host: the call is refused that one, as
    // by a host that is behind the Merk it hosts.
    let caller = tx.txn.scope().expect("a scope");
    let other = tx.txn.scope().expect("a scope beside it");
    write_under(&world, &tx, &other).expect("the Merk is free");
    assert!(!other.is_laid_out());
    let calls = db.counts.rule_calls.load(Ordering::Relaxed);
    let refused = caller.evaluate(sub(TOP), &mut Rules(&db.counts.rule_calls));
    match refused {
        Err(TxnError::Hash(HashError::HostBehind { host })) => {
            let cell = (caller.node(sub(TOP), &host_of(0), handle_of)).expect("a read");
            assert_eq!(Some(host), cell.map(|handle| handle.address()));
        }
        other => panic!("another scope writes the Merk under the host: {other:?}"),
    }
    assert_eq!(db.counts.rule_calls.load(Ordering::Relaxed), calls);
    other.rollback();
    tx.model.borrow_mut().writes.truncate(model_writes);

    // With no scope beside it that holds what it comes to, the call
    // makes the hashes, and holds the Merks: one for writing, one for
    // reading.
    let hash = evaluate(&world, &tx, &caller, TOP, &[TOP, UNDER[1]]);
    assert_eq!(hash.expect("the hashes are made"), root_hash);
    let other = tx.txn.scope().expect("a scope");
    let read = |name: u8| other.node(sub(name), &key(2), |view| view.is_some());
    assert!(read(UNDER[0]).expect("two scopes read one Merk"));
    match read(UNDER[1]) {
        Err(TxnError::ScopeOverlap { unit }) => assert_eq!(unit, Unit::data(sub(UNDER[1]))),
        other => panic!("the caller wrote the Merk: {other:?}"),
    }
    overlap(write_under(&world, &tx, &other).map(|_| None), UNDER[0]);
    other.rollback();
    caller.merge();
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

/// What the scope beside the caller does in
/// [`a_call_is_answered_alike_whether_the_scope_beside_it_is_laid_out`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Beside {
    /// It writes the Merk under the first host, which has nodes, and
    /// marks the host pending.
    WritesBelow,
    /// The same, of a Merk that had no node: the host's cell names no
    /// root of it.
    WritesTheFirstNode,
    /// It marks the second host pending and writes nothing.
    MarksAlone,
}

/// What a call answers beside another open scope is the same whether
/// that scope's writes are laid out on the transaction's pages or kept
/// in the write set (SC4, SC7). The call names the Merk that a host
/// points into by the subtree directory and holds it before it reads it,
/// so a Merk that the other scope writes refuses it, as a host that is
/// behind does. It reads nothing of a Merk that the host's cell names no
/// root of. And it is shown the pending marks as its scope is (SC9): a
/// mark of the other scope refuses it nothing.
#[test]
fn a_call_is_answered_alike_whether_the_scope_beside_it_is_laid_out() {
    let cases = [
        Beside::WritesBelow,
        Beside::WritesTheFirstNode,
        Beside::MarksAlone,
    ];
    for (beside, laid) in cases
        .into_iter()
        .flat_map(|case| [(case, true), (case, false)])
    {
        let (db, versions) = Db::new(128);
        let mut world = World::new(&db, &versions);
        grove(&mut world);
        if beside == Beside::WritesTheFirstNode {
            // The first Merk is cleared, and its host names no root.
            let tx = world.begin();
            let mut top = world.merk(&tx, TOP, |items| {
                drop(items.insert(host_of(0), b"tree, cleared".to_vec()))
            });
            top.hosted.insert(host_of(0), vec![sub(UNDER[0])]);
            let cleared = OwnedBatch {
                records: World::root_record(&top).into_iter().collect(),
                merks: vec![top],
                subtrees: vec![OwnedSubtree::Clear(sub(UNDER[0]))],
            };
            world.apply(&tx, &cleared).expect("the Merk is cleared");
            assert!(committed(world.commit(tx, &[])));
        }
        let tx = world.begin();
        let any = (tx.txn.node(sub(TOP), &host_of(2), handle_of)).expect("a read");
        let any = any.expect("a cell").address();
        // The scope that is opened first has its writes laid out.
        let first = tx.txn.scope().expect("a scope");
        let second = tx.txn.scope().expect("a scope beside it");
        let (other, caller) = match laid {
            true => (first, second),
            false => (second, first),
        };
        // The caller writes every host, with its value hash left.
        let top = left(&world, &tx, TOP, |items| {
            for at in 0..UNDER.len() {
                items.insert(host_of(at), b"tree, written again".to_vec());
            }
        });
        let through = batch(vec![top]).through(&caller);
        world.apply_in(&tx, &caller, &through).expect("the hosts");
        let marked = match beside {
            Beside::WritesBelow | Beside::WritesTheFirstNode => {
                write_under(&world, &tx, &other).expect("the Merk is free");
                host_of(0)
            }
            Beside::MarksAlone => host_of(1),
        };
        other.mark_pending(sub(TOP), &marked).expect("a mark");
        assert_eq!(other.is_laid_out(), laid, "{beside:?}");
        assert!(other.is_pending(sub(TOP), &marked));
        assert!(!caller.is_pending(sub(TOP), &marked));

        // The hosts that the pages have as behind, for the writes of the
        // scopes: the one that the other scope marked, where its writes
        // are laid out. The call takes none of them from the pages.
        let behind = |tx: &Tx<'_>| tx.txn.behind(any).expect("frames").0.len();
        assert_eq!(behind(&tx), usize::from(laid), "{beside:?}");

        let calls = db.counts.rule_calls.load(Ordering::Relaxed);
        let mut rules = Hosting {
            rules: Rules(&db.counts.rule_calls),
            hosted: BTreeMap::new(),
        };
        let made = caller.evaluate(sub(TOP), &mut rules);
        assert_eq!(behind(&tx), usize::from(laid), "{beside:?}, after the call");
        match (beside, made) {
            (Beside::WritesBelow, Err(TxnError::Hash(HashError::HostBehind { host }))) => {
                let cell = (caller.node(sub(TOP), &host_of(0), handle_of)).expect("a read");
                assert_eq!(Some(host), cell.map(|handle| handle.address()), "{laid}");
                assert_eq!(db.counts.rule_calls.load(Ordering::Relaxed), calls);
            }
            (Beside::WritesTheFirstNode | Beside::MarksAlone, Ok(Some(hash))) => {
                assert_eq!(Some(hash), expected(&world, &tx, TOP), "{beside:?}, {laid}");
                // Each host is hashed with the roots its cell names: none
                // for the Merk that had no node, whatever the other scope
                // wrote there since.
                let committed = |at: usize| {
                    let unit = Unit::data(sub(UNDER[at]));
                    let records = world.model.records.iter();
                    let own = records.filter(|((own, _), _)| *own == unit);
                    let view = own.map(|((_, key), value)| (key.clone(), value.clone()));
                    hash_of_items(&Committed::items(&view.collect()))
                };
                let roots = [0, 1, 2].map(|at| match (beside, at) {
                    (Beside::WritesTheFirstNode, 0) => None,
                    _ => committed(at),
                });
                for (at, root) in roots.into_iter().enumerate() {
                    let given = rules.hosted.get(&host_of(at));
                    assert_eq!(given, Some(&vec![root]), "{beside:?}, {laid}, {at}");
                }
            }
            (_, other) => panic!("{beside:?}, {laid}: {other:?}"),
        }
        other.rollback();
        caller.rollback();
        assert_eq!(db.private_ids(), 0, "{beside:?}, {laid}");
    }
}

/// A call that is refused under a host is refused alike, and leaves its
/// scope with the same claims, whether the writes of the scope beside it
/// are laid out or kept in the write set. The other scope takes the Merk
/// under a host away, or puts another in its place, and writes nothing
/// of the host's Merk. With its writes kept the call finds the Merk that
/// was there, which that scope holds; with them laid out it finds none,
/// or the other one. Each is the refusal of a host that is behind, and
/// names no Merk. The caller holds the read of the Merk it was asked for
/// and no write: a third scope reads that Merk and is refused a write of
/// it.
#[test]
fn a_call_that_is_refused_holds_the_same_claims_in_either_layout() {
    const OTHER: u8 = ENTERED + 44;
    let mut left_by = Vec::new();
    let cases = [(false, false), (false, true), (true, false), (true, true)];
    for (replaced, laid) in cases {
        let (db, versions) = Db::new(128);
        let mut world = World::new(&db, &versions);
        grove(&mut world);
        let tx = world.begin();
        // The hosts are written with their value hashes left, by a scope
        // that merges.
        let top = left(&world, &tx, TOP, |items| {
            for at in 0..UNDER.len() {
                items.insert(host_of(at), b"tree, written again".to_vec());
            }
        });
        world.apply(&tx, &batch(vec![top])).expect("the hosts");
        let caller = tx.txn.scope().expect("a scope");
        let other = tx.txn.scope().expect("a scope beside it");
        let mut dropped = OwnedBatch {
            subtrees: vec![OwnedSubtree::Drop(sub(UNDER[1]))],
            ..OwnedBatch::default()
        };
        if replaced {
            // Another Merk is entered in its place, with a node.
            let host = (sub(TOP), host_of(1), 0);
            (dropped.subtrees).push(OwnedSubtree::Enter(sub(OTHER), Some(host)));
            let nodes = [(key(1), vec![9; 30])].into_iter().collect();
            let merk = OwnedMerk::change(sub(OTHER), &BTreeMap::new(), &nodes);
            dropped.merks.push(OwnedMerk {
                anchored: true,
                ..merk
            });
        }
        let model_writes = tx.model.borrow().writes.len();
        (world.apply_in(&tx, &other, &dropped.through(&other))).expect("the drop");
        other.mark_pending(sub(TOP), &host_of(1)).expect("a mark");
        if laid {
            other.lay_out().expect("the drop is laid out");
        }
        assert_eq!(other.is_laid_out(), laid);

        let calls = db.counts.rule_calls.load(Ordering::Relaxed);
        let refused = caller.evaluate(sub(TOP), &mut Rules(&db.counts.rule_calls));
        match refused {
            Err(TxnError::Hash(HashError::HostBehind { host })) => {
                let cell = (caller.node(sub(TOP), &host_of(1), handle_of)).expect("a read");
                assert_eq!(Some(host), cell.map(|handle| handle.address()));
            }
            other => panic!("{replaced}, {laid}: what is under the host is not read: {other:?}"),
        }
        assert_eq!(db.counts.rule_calls.load(Ordering::Relaxed), calls);

        // What a third scope may do tells what the caller holds.
        let third = tx.txn.scope().expect("a third scope");
        let mut held = Vec::new();
        for name in [TOP, UNDER[0], UNDER[2]] {
            let read = third.node(sub(name), &key(2), |view| view.is_some());
            let merk = left(&world, &tx, name, |items| {
                drop(items.insert(key(72), vec![7; 30]))
            });
            let merk = OwnedMerk {
                anchored: name != TOP,
                ..merk
            };
            let wrote = OwnedBatch {
                merks: vec![merk],
                ..OwnedBatch::default()
            };
            let wrote = world.apply_in(&tx, &third, &wrote.through(&third));
            held.push((name, read.is_ok(), wrote.is_ok()));
        }
        // The caller read the Merk it was asked for, and wrote nothing.
        assert_eq!(held[0], (TOP, true, false), "{replaced}, {laid}");
        left_by.push(held);
        tx.model.borrow_mut().writes.truncate(model_writes);
        third.rollback();
        other.rollback();
        caller.rollback();
    }
    assert_eq!(left_by[0], left_by[1], "a drop: kept, and laid out");
    assert_eq!(
        left_by[2], left_by[3],
        "another in its place: kept, and laid out"
    );
    assert_eq!(left_by[0], left_by[2], "with and without the other Merk");
}

/// The pending marks are asked as the caller's scope is shown them, and
/// the changes that the scope made are gone through once for the hosts
/// of a Merk, and not once for each node whose value hash is stale.
#[test]
fn the_marks_of_the_callers_scope_are_gone_through_once() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    grove(&mut world);
    let tx = world.begin();
    let scope = tx.txn.scope().expect("a scope");
    // Two Merks under hosts and the hosts are written, with every hash
    // left: the walk comes to hosts at the top, stops for the Merks under
    // them, and goes on to nodes with a stale value hash there.
    let changed = below(&world, &tx, &[0, 1], 1);
    let stale: Vec<usize> = changed.merks.iter().map(|merk| merk.nodes.len()).collect();
    assert!(stale.iter().all(|nodes| *nodes > 1), "{stale:?}");
    (world.apply_in(&tx, &scope, &changed.through(&scope))).expect("the change");
    // The scope's changes of marks: one that it set and took off again,
    // on a host, and others elsewhere.
    scope.mark_pending(sub(TOP), &host_of(0)).expect("a mark");
    scope.clear_pending(sub(TOP), &host_of(0)).expect("no mark");
    for other in 0..7 {
        let elsewhere = format!("elsewhere-{other}");
        (scope.clear_pending(sub(TOP), elsewhere.as_bytes())).expect("no mark");
    }
    crate::txn::txn::counted_marks();
    let hash = evaluate(&world, &tx, &scope, TOP, &[TOP, UNDER[0], UNDER[1]]);
    assert_eq!(hash.expect("no host is marked"), expected(&world, &tx, TOP));
    assert_eq!(crate::txn::txn::counted_marks(), 9);

    // The newest change of an element counts: a mark that is set again
    // refuses the call.
    let top = left(&world, &tx, TOP, |items| {
        drop(items.insert(host_of(0), b"tree, once more".to_vec()))
    });
    let through = batch(vec![top]).through(&scope);
    world.apply_in(&tx, &scope, &through).expect("the host");
    scope.mark_pending(sub(TOP), &host_of(0)).expect("a mark");
    let refused = scope.evaluate(sub(TOP), &mut Rules(&db.counts.rule_calls));
    let behind = matches!(refused, Err(TxnError::Hash(HashError::HostBehind { .. })));
    assert!(behind, "{refused:?}");
    assert_eq!(crate::txn::txn::counted_marks(), 10);
    scope.clear_pending(sub(TOP), &host_of(0)).expect("no mark");
    let hash = evaluate(&world, &tx, &scope, TOP, &[TOP]);
    assert_eq!(hash.expect("no host is marked"), expected(&world, &tx, TOP));

    // A mark that a scope leaves when it merges is the transaction's: it
    // is shown to every scope that is opened afterwards, and to a call
    // through the transaction, and refuses each until a scope takes it
    // off.
    let top = left(&world, &tx, TOP, |items| {
        drop(items.insert(host_of(2), b"tree, a third time".to_vec()))
    });
    let through = batch(vec![top]).through(&scope);
    world.apply_in(&tx, &scope, &through).expect("the host");
    scope.mark_pending(sub(TOP), &host_of(2)).expect("a mark");
    scope.merge();
    assert!(tx.txn.is_pending(sub(TOP), &host_of(2)));
    let behind = |refused: Result<Option<Hash>, TxnError>| {
        let behind = matches!(refused, Err(TxnError::Hash(HashError::HostBehind { .. })));
        assert!(behind, "the mark of a merged scope: {refused:?}");
    };
    let calls = db.counts.rule_calls.load(Ordering::Relaxed);
    behind(tx.txn.evaluate(sub(TOP), &mut Rules(&db.counts.rule_calls)));
    let later = tx.txn.scope().expect("a scope");
    behind(later.evaluate(sub(TOP), &mut Rules(&db.counts.rule_calls)));
    assert_eq!(db.counts.rule_calls.load(Ordering::Relaxed), calls);
    later.clear_pending(sub(TOP), &host_of(2)).expect("no mark");
    let hash = evaluate(&world, &tx, &later, TOP, &[TOP]);
    assert_eq!(hash.expect("no host is marked"), expected(&world, &tx, TOP));
    later.merge();
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

/// A transaction whose writes cannot be laid out has no frames, and the
/// call is refused with what refused the layout, whichever Merk is asked
/// for, through the transaction and through a scope. Here a scope that
/// kept its writes in the write set merged a change set whose node links
/// a child that is not there (SC6). No hash is made, and once a rollback
/// has taken that write out the call makes the hashes as before.
#[test]
fn a_transaction_whose_writes_cannot_be_laid_out_refuses_the_call() {
    const OTHER: u8 = 5;

    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(126);
    planted(&mut world, &mut rng);
    let grown = world.edit_outside(
        OTHER,
        |items| {
            for index in 0..5 {
                items.insert(key(index), vec![5; 30]);
            }
        },
        &[],
    );
    assert!(committed(grown));
    let calls = || db.counts.rule_calls.load(Ordering::Relaxed);
    let no_child = |refused: Result<Option<Hash>, TxnError>| match refused {
        Err(TxnError::Batch(BatchError::NoChild { subtree })) => assert_eq!(subtree, sub(OTHER)),
        other => panic!("the layout is refused for the child that is not there: {other:?}"),
    };

    let tx = world.begin();
    let merk = changed(&world, &tx, &mut rng, 1, 0);
    world.apply(&tx, &batch(vec![merk])).expect("the change");
    let (root_hash, stale) = (expected(&world, &tx, 1), stale_in(&world, &tx, 1));
    tx.txn.set_savepoint().expect("no scope is open");

    // Beside another scope, a scope keeps a change set in the write set
    // whose node links a child that its Merk does not have, and merges.
    let first = tx.txn.scope().expect("a scope");
    let beside = tx.txn.scope().expect("a scope beside it");
    let mut broken = world.merk(&tx, OTHER, |items| {
        drop(items.insert(key(1), b"x".to_vec()))
    });
    let link = (broken.nodes.iter_mut())
        .find_map(|(_, node, _)| node.left.as_mut().or(node.right.as_mut()))
        .expect("a node with a child");
    link.key = b"no such node".to_vec();
    let broken = OwnedBatch {
        merks: vec![broken],
        ..OwnedBatch::default()
    };
    (broken.through(&beside))
        .with(|batch| beside.apply(batch))
        .expect("kept in the write set");
    assert!(!beside.is_laid_out());
    beside.merge();
    first.merge();

    // The call is refused as the layout is, for the Merk with the stale
    // hashes, for the Merk of the write that is refused, and for one
    // that nothing was written in.
    let before = calls();
    for name in [1, OTHER, 2] {
        no_child(
            tx.txn
                .evaluate(sub(name), &mut Rules(&db.counts.rule_calls)),
        );
    }
    assert_eq!(tx.txn.stats().base, None, "the transaction has no frames");
    let later = tx.txn.scope().expect("a scope");
    no_child(later.evaluate(sub(1), &mut Rules(&db.counts.rule_calls)));
    later.rollback();
    assert_eq!(calls(), before);

    // The rollback takes the write out: the frames are made again, every
    // hash that was stale is stale, and the call makes them.
    tx.txn.rollback_to_savepoint().expect("a savepoint is set");
    assert_eq!(stale_in(&world, &tx, 1), stale);
    assert_eq!(evaluate_merged(&world, &tx, 1), root_hash);
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

/// Rules that refuse the call after `left` calls.
struct Refusing<'a> {
    rules: Rules<'a>,
    left: usize,
}

impl Refusing<'_> {
    fn take(&mut self) -> Result<(), Refusal> {
        match self.left.checked_sub(1) {
            Some(left) => {
                self.left = left;
                Ok(())
            }
            None => Err("a rule of the test refuses".into()),
        }
    }
}

impl HashRules for Refusing<'_> {
    fn value_hash(
        &mut self,
        node: NodeRef<'_>,
        value: &[u8],
        hosted: &[Option<Hash>],
    ) -> Result<Hash, Refusal> {
        self.take()?;
        self.rules.value_hash(node, value, hosted)
    }

    fn kv_hash(&mut self, key: &[u8], value_hash: &Hash) -> Result<Hash, Refusal> {
        self.take()?;
        self.rules.kv_hash(key, value_hash)
    }

    fn node_hash(&mut self, node: NodeRef<'_>) -> Result<Hash, Refusal> {
        self.take()?;
        self.rules.node_hash(node)
    }
}

/// A rule that refuses a hash ends the call with its error, and adds
/// nothing (WT10): every hash that was stale is stale, with the bytes its
/// writer stated, wherever the evaluation stopped. Where a hash was
/// stored before the error, the frames are made again from the write
/// set, and have the hashes that an earlier call made.
#[test]
fn a_call_that_a_rule_refuses_adds_nothing() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(118);
    planted(&mut world, &mut rng);
    let tx = world.begin();
    // The hashes of another Merk, which an earlier call made.
    let other = left(&world, &tx, 5, |items| {
        for index in 0..9 {
            items.insert(key(index), value(&mut rng, 40));
        }
    });
    world.apply(&tx, &batch(vec![other])).expect("another Merk");
    let made_before = evaluate_merged(&world, &tx, 5);
    assert!(made_before.is_some());
    let merk = changed(&world, &tx, &mut rng, 1, 0);
    let to_make = calls_for(&merk) as usize + 1;
    world.apply(&tx, &batch(vec![merk])).expect("the change");
    let stale = stale_in(&world, &tx, 1);
    let root_hash = expected(&world, &tx, 1);
    let held = held_by(&tx);

    let mut rebuilt = 0;
    for left in [0, 1, 2, 5, to_make / 2, to_make - 2, to_make - 1] {
        let calls = std::sync::atomic::AtomicU64::new(0);
        let mut rules = Refusing {
            rules: Rules(&calls),
            left,
        };
        let refused = tx.txn.evaluate(sub(1), &mut rules);
        match refused {
            Err(TxnError::Hash(HashError::Rule(error))) => {
                assert_eq!(error.to_string(), "a rule of the test refuses")
            }
            other => panic!("the rules refuse after {left} calls: {other:?}"),
        }
        assert_eq!(calls.load(Ordering::Relaxed), left as u64);
        // The read makes the frames again where they were given up.
        assert_eq!(stale_in(&world, &tx, 1), stale, "after {left} calls");
        assert_eq!(stale_in(&world, &tx, 5), BTreeSet::new(), "{left}");
        for key in &stale {
            let (_, value_hash, kv_hash, _) = shown(&tx, 1, key);
            assert_eq!((value_hash, kv_hash), (UNMADE, UNMADE), "{key:?}, {left}");
        }
        // The frames were made again where a hash was stored before the
        // error, and are the ones they were otherwise. No hash is stored
        // before the first call. Before the last, which makes the root's
        // own hash, every one is.
        let now = held_by(&tx);
        let again = now.rebuilds - held.rebuilds - rebuilt;
        match left {
            0 => assert_eq!(again, 0),
            _ if left == to_make - 1 => assert_eq!(again, 1),
            _ => assert!(again <= 1, "after {left} calls"),
        }
        rebuilt += again;
        let same = TxnStats {
            rebuilds: held.rebuilds,
            tree_pages: held.tree_pages,
            ..now
        };
        assert_eq!(same, held, "after {left} calls");
    }
    assert!(rebuilt >= 1);

    // With rules that make every hash, the call makes them all.
    let calls = std::sync::atomic::AtomicU64::new(0);
    let mut rules = Refusing {
        rules: Rules(&calls),
        left: to_make,
    };
    assert_eq!(
        tx.txn.evaluate(sub(1), &mut rules).expect("a hash"),
        root_hash
    );
    assert_eq!(calls.load(Ordering::Relaxed), to_make as u64);
    assert_eq!(tx.txn.stats().stale_nodes, 0);
    let other = tx.txn.evaluate(sub(5), &mut Rules(&calls));
    assert_eq!(other.expect("a hash"), made_before);
    assert!(committed(db.commit(&versions, &tx.txn).expect("a commit")));
}

/// Rules that panic at their call number `at`.
struct Panicking<'a> {
    rules: Rules<'a>,
    at: u64,
}

impl Panicking<'_> {
    fn count(&mut self) {
        assert!(
            self.rules.0.load(Ordering::Relaxed) != self.at,
            "a rule of the test panics"
        );
    }
}

impl HashRules for Panicking<'_> {
    fn value_hash(
        &mut self,
        node: NodeRef<'_>,
        value: &[u8],
        hosted: &[Option<Hash>],
    ) -> Result<Hash, Refusal> {
        self.count();
        self.rules.value_hash(node, value, hosted)
    }

    fn kv_hash(&mut self, key: &[u8], value_hash: &Hash) -> Result<Hash, Refusal> {
        self.count();
        self.rules.kv_hash(key, value_hash)
    }

    fn node_hash(&mut self, node: NodeRef<'_>) -> Result<Hash, Refusal> {
        self.count();
        self.rules.node_hash(node)
    }
}

/// Rules that panic leave the transaction usable, as any operation that
/// panics does: the frames may hold hashes that the write set does not,
/// and are made again from it. Every hash that was stale is stale, and
/// the next call makes them.
#[test]
fn a_call_whose_rules_panic_adds_nothing() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(123);
    planted(&mut world, &mut rng);
    let tx = world.begin();
    let merk = changed(&world, &tx, &mut rng, 1, 0);
    let to_make = calls_for(&merk) + 1;
    world.apply(&tx, &batch(vec![merk])).expect("the change");
    let stale = stale_in(&world, &tx, 1);
    let root_hash = expected(&world, &tx, 1);
    let held = held_by(&tx);

    for at in [0, to_make / 2, to_make - 1] {
        let calls = std::sync::atomic::AtomicU64::new(0);
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut rules = Panicking {
                rules: Rules(&calls),
                at,
            };
            tx.txn.evaluate(sub(1), &mut rules)
        }));
        assert!(panicked.is_err(), "the rules panic at their call {at}");
        assert_eq!(calls.load(Ordering::Relaxed), at);
        assert_eq!(stale_in(&world, &tx, 1), stale, "after {at} calls");
        for key in &stale {
            assert_eq!(shown(&tx, 1, key).1, UNMADE, "{key:?}, {at}");
        }
        let now = held_by(&tx);
        assert_eq!(
            (now.writes, now.stale_nodes),
            (held.writes, held.stale_nodes)
        );
    }
    // The frames were made again after each.
    assert_eq!(held_by(&tx).rebuilds, held.rebuilds + 3);
    assert_eq!(evaluate_merged(&world, &tx, 1), root_hash);
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

/// A subtree that the test enters itself, with no host.
const OWN: u8 = ENTERED + 35;

/// What becomes of the made hashes when their subtree is cleared, is
/// dropped and is entered again, and when the transaction is rolled
/// back: they go with the nodes they are of, as the nodes' other writes.
#[test]
fn the_made_hashes_go_with_a_subtree_that_leaves_and_with_the_transaction() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(119);
    planted(&mut world, &mut rng);
    let mut rules = Rules(&db.counts.rule_calls);
    let nodes = |rng: &mut Rng, round: usize| -> Vec<(Vec<u8>, Vec<u8>)> {
        (0..10)
            .map(|index| (key(round * 100 + index), value(rng, 50)))
            .collect()
    };
    let not_entered = |refused: Result<Option<Hash>, TxnError>| match refused {
        Err(TxnError::Batch(BatchError::NotEntered { subtree })) => assert_eq!(subtree, sub(OWN)),
        other => panic!("a subtree that is not entered: {other:?}"),
    };
    let leaves = |write: OwnedSubtree| OwnedBatch {
        subtrees: vec![write],
        ..OwnedBatch::default()
    };

    let tx = world.begin();
    not_entered(tx.txn.evaluate(sub(OWN), &mut rules));
    // The subtree is entered with nodes whose hashes are left.
    let fill = nodes(&mut rng, 0);
    let merk = left(&world, &tx, OWN, |items| items.extend(fill));
    let entered = OwnedBatch {
        subtrees: vec![OwnedSubtree::Enter(sub(OWN), None)],
        ..batch(vec![merk])
    };
    world.apply(&tx, &entered).expect("the subtree is entered");
    let first = evaluate_merged(&world, &tx, OWN);
    assert_eq!(first, expected(&world, &tx, OWN));
    assert!(first.is_some());

    // A clear: the Merk has no node, and nothing of it is stale.
    world
        .apply(&tx, &leaves(OwnedSubtree::Clear(sub(OWN))))
        .expect("the subtree is cleared");
    assert_eq!(
        tx.txn.evaluate(sub(OWN), &mut rules).expect("no node"),
        None
    );
    let fill = nodes(&mut rng, 1);
    let merk = left(&world, &tx, OWN, |items| items.extend(fill));
    world
        .apply(&tx, &batch(vec![merk]))
        .expect("it is filled again");
    let second = evaluate_merged(&world, &tx, OWN);
    assert_eq!(second, expected(&world, &tx, OWN));
    assert!(second.is_some() && second != first);

    // A drop: the subtree is not entered. Entered again, it is another
    // Merk, with the hashes its writes leave and none of before.
    world
        .apply(&tx, &leaves(OwnedSubtree::Drop(sub(OWN))))
        .expect("the subtree is dropped");
    not_entered(tx.txn.evaluate(sub(OWN), &mut rules));
    let fill = nodes(&mut rng, 2);
    let merk = left(&world, &tx, OWN, |items| items.extend(fill));
    let stale = merk.nodes.len();
    let entered = OwnedBatch {
        subtrees: vec![OwnedSubtree::Enter(sub(OWN), None)],
        ..batch(vec![merk])
    };
    world
        .apply(&tx, &entered)
        .expect("the subtree is entered again");
    assert_eq!(stale_in(&world, &tx, OWN).len(), stale);
    assert_eq!(tx.txn.stats().stale_nodes, stale);
    let third = evaluate_merged(&world, &tx, OWN);
    assert_eq!(third, expected(&world, &tx, OWN));
    assert!(third.is_some() && third != second);

    // Frames that are made again go through all of it again.
    let rebuilds = tx.txn.stats().rebuilds;
    world.edit_outside(2, |items| drop(items.insert(key(1), vec![1; 30])), &[&tx]);
    world.check(&tx);
    let stats = tx.txn.stats();
    assert_eq!((stats.rebuilds, stats.stale_nodes), (rebuilds + 1, 0));
    assert_eq!(
        tx.txn.evaluate(sub(OWN), &mut rules).expect("a hash"),
        third
    );
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();

    // A transaction that is rolled back holds nothing of a call, and
    // reads the committed Merk.
    let tx = world.begin();
    let merk = changed(&world, &tx, &mut rng, 1, 0);
    world.apply(&tx, &batch(vec![merk])).expect("the change");
    let before = evaluate_merged(&world, &tx, 1);
    tx.txn.rollback().expect("no scope is open");
    tx.forget();
    let nothing = TxnStats {
        rebuilds: 0,
        handles_followed: 0,
        places_followed: 0,
        keyed_reads: 0,
        ..tx.txn.stats()
    };
    assert_eq!(nothing, TxnStats::default());
    let after = tx.txn.evaluate(sub(1), &mut rules).expect("a hash");
    assert_eq!(after, expected(&world, &tx, 1));
    assert!(after.is_some() && after != before);
    assert_eq!(db.private_ids(), 0);
}

/// The made hashes go with a Merk that is moved to another prefix: its
/// nodes are read under the new prefix with them, also from frames that
/// are made again, and a call under the new prefix makes what is written
/// there afterwards.
#[test]
fn the_made_hashes_go_with_a_merk_that_is_moved() {
    use super::moves::{edit_at, rename, A, A_IN, B, B_IN};
    const MOVED_TOP: u8 = super::moves::TOP;

    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    super::moves::grove(&mut world, b"a", A, A_IN);
    let calls = || db.counts.rule_calls.load(Ordering::Relaxed);
    let evaluated = || db.counts.stale_nodes.load(Ordering::Relaxed);

    let tx = world.begin();
    let mut before = edit_at(&world, &tx, &[(A, b"a")], |items| {
        items.insert(b"k3".to_vec(), vec![6; 50]);
        items.insert(b"k1".to_vec(), vec![5; 45]);
    });
    before.merks.iter_mut().for_each(leave);
    world
        .apply(&tx, &before)
        .expect("a change under the source");
    let scope = tx.txn.scope().expect("a scope");
    let hash = evaluate(&world, &tx, &scope, MOVED_TOP, &[MOVED_TOP, A]);
    assert_eq!(hash.expect("a hash"), expected(&world, &tx, MOVED_TOP));
    scope.merge();
    let under_a = expected(&world, &tx, A);

    let moved = rename(&world, &tx, b"a", b"b", &[(A, B), (A_IN, B_IN)]);
    world.apply(&tx, &moved).expect("the subtree is moved");
    world.check(&tx);
    assert_eq!(stale_in(&world, &tx, B), BTreeSet::new());
    let before = calls();
    assert_eq!(evaluate_merged(&world, &tx, B), under_a);
    assert_eq!(calls() - before, 1);

    // A change under the new prefix, with its hashes left, and the call.
    let mut after = edit_at(&world, &tx, &[(B, b"b")], |items| {
        items.insert(b"k4".to_vec(), vec![7; 60]);
        items.remove(&b"k2"[..]);
    });
    after.merks.iter_mut().for_each(leave);
    world.apply(&tx, &after).expect("a change under the target");
    assert!(!stale_in(&world, &tx, B).is_empty());
    let scope = tx.txn.scope().expect("a scope");
    let hash = evaluate(&world, &tx, &scope, MOVED_TOP, &[MOVED_TOP, B]);
    assert_eq!(hash.expect("a hash"), expected(&world, &tx, MOVED_TOP));
    scope.merge();
    world.check(&tx);

    let rebuilds = tx.txn.stats().rebuilds;
    world.edit_outside(2, |items| drop(items.insert(key(1), vec![1; 30])), &[&tx]);
    world.check(&tx);
    let stats = tx.txn.stats();
    assert_eq!((stats.rebuilds, stats.stale_nodes), (rebuilds + 1, 0));
    let before = (calls(), evaluated());
    assert!(committed(world.commit(tx, &[])));
    assert_eq!((calls(), evaluated()), before);
    world.check_committed();
}

/// A stale hash that no stale hash leads to is found in a Merk that was
/// moved after the write that left it: the call under the new prefix is
/// given the puts that were made under the old one, and makes them all.
#[test]
fn the_call_makes_what_was_left_stale_before_a_merk_was_moved() {
    use super::moves::{edit_at, rename, A, A_IN, B, B_IN};

    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    super::moves::grove(&mut world, b"a", A, A_IN);
    let calls = || db.counts.rule_calls.load(Ordering::Relaxed);
    let evaluated = || db.counts.stale_nodes.load(Ordering::Relaxed);

    let tx = world.begin();
    let mut left = edit_at(&world, &tx, &[(A, b"a")], |items| {
        for index in 0..9 {
            items.insert(format!("k{index}").into_bytes(), vec![6; 50]);
        }
    });
    left.merks.iter_mut().for_each(leave);
    world.apply(&tx, &left).expect("a change under the source");
    // The root is written again by a writer that makes its hashes.
    let root = model::records(&items(&world, &tx, A)).0.expect("a root");
    let above = edit_at(&world, &tx, &[(A, b"a")], |items| {
        items.get_mut(&root).expect("the root").push(1);
    });
    world.apply(&tx, &above).expect("the root is written");
    let stale = stale_in(&world, &tx, A);
    assert!(stale.len() > 3 && !stale.contains(&root), "{stale:?}");

    let moved = rename(&world, &tx, b"a", b"b", &[(A, B), (A_IN, B_IN)]);
    world.apply(&tx, &moved).expect("the subtree is moved");
    assert_eq!(stale_in(&world, &tx, B), stale);
    assert_eq!(tx.txn.stats().stale_nodes, stale.len());

    let root_hash = expected(&world, &tx, B);
    let scope = tx.txn.scope().expect("a scope");
    held::compared();
    let hash = evaluate(&world, &tx, &scope, B, &[B]);
    assert_eq!(hash.expect("the hashes are made"), root_hash);
    scope.merge();
    assert_eq!(held::compared().evaluated, stale.len() as u64);
    assert_eq!(stale_in(&world, &tx, B), BTreeSet::new());
    assert_eq!(tx.txn.stats().stale_nodes, 0);
    world.check(&tx);

    // Frames that are made again have the made hashes under the new
    // prefix, and the commit makes none.
    let rebuilds = tx.txn.stats().rebuilds;
    world.edit_outside(2, |items| drop(items.insert(key(1), vec![1; 30])), &[&tx]);
    world.check(&tx);
    let stats = tx.txn.stats();
    assert_eq!((stats.rebuilds, stats.stale_nodes), (rebuilds + 1, 0));
    let before = (calls(), evaluated());
    assert_eq!(evaluate_merged(&world, &tx, B), root_hash);
    assert_eq!(calls() - before.0, 1);
    let before = (calls(), evaluated());
    assert!(committed(world.commit(tx, &[])));
    assert_eq!((calls(), evaluated()), before);
    world.check_committed();
}

/// A subtree of a tree type that keeps flat records can hold nodes and
/// flat records both, where its writer says so ([`Scope::apply_by_kind`],
/// `transactions.md` ST1). The operations of a call are laid out again
/// there as the nodes they are: frames that are made again take them.
#[test]
fn the_made_hashes_are_laid_out_again_beside_flat_records() {
    const FLAT: u8 = 7;
    const FLAT_TYPE: &[u8] = b"flat";
    let kinds: FlatKinds = |tree_type| tree_type == FLAT_TYPE;

    let (db, versions) = Db::new(96);
    db.check_kinds_by(kinds);
    let mut world = World::new(&db, &versions);
    let anew = OwnedSubtree::Convert(sub(FLAT), FLAT_TYPE.to_vec(), None);
    let stated = OwnedBatch {
        subtrees: vec![anew],
        ..OwnedBatch::default()
    };
    assert!(committed(world.write_outside(&stated, &[])));

    let tx = world.begin();
    let by_kind = |made: &OwnedBatch, nodes: bool| {
        let scope = tx.txn.scope().expect("a scope");
        let first_write = world.latest().get();
        let read = made.through(&scope);
        read.with(|made| scope.apply_by_kind(made, kinds))
            .expect("the tree type takes both");
        scope.merge();
        if nodes {
            world.note(&tx, made, first_write);
        }
    };
    // Nodes with their hashes left, and then a flat record beside them.
    let merk = left(&world, &tx, FLAT, |items| {
        for index in 0..8 {
            items.insert(key(index), vec![index as u8; 40]);
        }
    });
    by_kind(&batch(vec![merk]), true);
    let record = OwnedBatch {
        records: vec![put(Unit::data(sub(FLAT)), b"record", b"flat")],
        ..OwnedBatch::default()
    };
    by_kind(&record, false);
    let root_hash = expected(&world, &tx, FLAT);
    assert_eq!(evaluate_merged(&world, &tx, FLAT), root_hash);

    // A commit beside the transaction: its writes are laid out again.
    let rebuilds = tx.txn.stats().rebuilds;
    let outside = world.begin();
    world.edit(&outside, 2, |items| drop(items.insert(key(1), vec![1; 30])));
    assert!(committed(
        db.commit(&versions, &outside.txn).expect("a commit")
    ));
    for index in 0..8 {
        let (stale, value_hash, _, record) = shown(&tx, FLAT, &key(index));
        assert!(stale.is_empty() && value_hash != UNMADE && !record);
    }
    let stats = tx.txn.stats();
    assert_eq!((stats.rebuilds, stats.stale_nodes), (rebuilds + 1, 0));
    let made = tx
        .txn
        .evaluate(sub(FLAT), &mut Rules(&db.counts.rule_calls));
    assert_eq!(made.expect("a hash"), root_hash);
    let evaluated = db.counts.stale_nodes.load(Ordering::Relaxed);
    assert!(committed(db.commit(&versions, &tx.txn).expect("a commit")));
    assert_eq!(db.counts.stale_nodes.load(Ordering::Relaxed), evaluated);
}

/// Histories of changes that leave hashes, calls, savepoints, rollbacks
/// to them and commits beside the transaction: at every step the nodes
/// with a stale hash are those that were written since the last call for
/// their Merk that still stands, every call gives the root hash of the
/// model, and the commit makes what is left and nothing else. Each store
/// is read by the physical checker when it is dropped, which makes every
/// hash again with the rules.
#[test]
fn histories_of_changes_calls_and_rollbacks_leave_what_the_rules_make() {
    for seed in 0..10 {
        let (db, versions) = Db::new(128);
        let mut world = World::new(&db, &versions);
        let mut rng = Rng::new(900 + seed);
        let merks = [1u8, 3, 5];
        for name in merks {
            let made = world.edit_outside(
                name,
                |items| {
                    for index in 0..25 {
                        items.insert(key(index), value(&mut rng, 40));
                    }
                },
                &[],
            );
            assert!(committed(made));
        }
        let evaluated = || db.counts.stale_nodes.load(Ordering::Relaxed);
        let tx = world.begin();
        // The nodes that have a stale hash, by Merk, and what they were
        // at each savepoint, with the writes the model had.
        let mut stale = StaleKeys::new();
        let mut saved: Vec<(usize, StaleKeys)> = Vec::new();
        let (mut calls, mut rollbacks, mut rebases) = (0, 0, 0);
        for step in 0..60 {
            let name = merks[rng.below(merks.len())];
            match rng.below(12) {
                0..=4 => {
                    let merk = left(&world, &tx, name, |items| {
                        for _ in 0..1 + rng.below(4) {
                            items.insert(key(rng.below(60)), value(&mut rng, 30 + step));
                        }
                        if rng.chance(40) {
                            items.remove(&key(rng.below(60)));
                        }
                    });
                    let of = stale.entry(name).or_default();
                    for gone in &merk.removed {
                        of.remove(gone);
                    }
                    of.extend(merk.nodes.iter().map(|node| node.0.clone()));
                    world.apply(&tx, &batch(vec![merk])).expect("a change");
                }
                5..=7 => {
                    let root_hash = expected(&world, &tx, name);
                    let hash = match rng.chance(50) {
                        true => evaluate_merged(&world, &tx, name),
                        false => {
                            // Through the transaction: the model is told
                            // by a scope that makes nothing more.
                            let made = tx
                                .txn
                                .evaluate(sub(name), &mut Rules(&db.counts.rule_calls));
                            let made = made.expect("the hashes are made");
                            let scope = tx.txn.scope().expect("a scope");
                            tell(&world, &tx, name, stale.get(&name), world.latest().get());
                            scope.merge();
                            made
                        }
                    };
                    assert_eq!(hash, root_hash, "seed {seed}, step {step}");
                    stale.remove(&name);
                    calls += 1;
                }
                8 => {
                    tx.txn.set_savepoint().expect("no scope is open");
                    saved.push((tx.model.borrow().writes.len(), stale.clone()));
                }
                9 => {
                    if let Some((writes, was)) = saved.pop() {
                        tx.txn.rollback_to_savepoint().expect("one is set");
                        tx.model.borrow_mut().writes.truncate(writes);
                        stale = was;
                        rollbacks += 1;
                    }
                }
                10 => {
                    let beside = |items: &mut Items| drop(items.insert(key(step), vec![9; 20]));
                    assert!(committed(world.edit_outside(8, beside, &[&tx])));
                    rebases += 1;
                }
                _ => {}
            }
            for name in merks {
                let of = stale.get(&name).cloned().unwrap_or_default();
                assert_eq!(
                    stale_in(&world, &tx, name),
                    of,
                    "seed {seed}, step {step}, {name}"
                );
            }
            let all: usize = stale.values().map(BTreeSet::len).sum();
            if tx.txn.stats().writes > 0 {
                assert_eq!(tx.txn.stats().stale_nodes, all, "seed {seed}, step {step}");
            }
        }
        assert!(
            calls > 5 && rebases > 0,
            "seed {seed}: {calls}, {rollbacks}, {rebases}"
        );
        let left_stale: usize = stale.values().map(BTreeSet::len).sum();
        let hashes = merks.map(|name| expected(&world, &tx, name));
        let before = evaluated();
        assert!(committed(world.commit(tx, &[])));
        assert_eq!(evaluated() - before, left_stale as u64, "seed {seed}");
        world.check_committed();
        let reader = world.begin();
        for (name, hash) in merks.into_iter().zip(hashes) {
            let published = reader
                .txn
                .evaluate(sub(name), &mut Rules(&db.counts.rule_calls));
            assert_eq!(published.expect("a hash"), hash, "seed {seed}, {name}");
        }
        assert_eq!(db.private_ids(), 0);
    }
}

/// A clear leaves the pointer of the host at the root that the Merk had,
/// until the host is written without it. The commit refuses that state
/// ([`BatchError::ClearedAlone`]), and so does the call, before a hash is
/// made: the value hash of such a host would be made from cells that are
/// of no Merk any more.
#[test]
fn a_host_that_points_at_the_root_of_a_cleared_merk_refuses_the_call() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    grove(&mut world);
    let calls = || db.counts.rule_calls.load(Ordering::Relaxed);
    let tx = world.begin();
    let changed = below(&world, &tx, &[0, 1], 1);
    world.apply(&tx, &changed).expect("the change");
    let stale = stale_in(&world, &tx, TOP);
    assert!(stale.contains(&host_of(0)));

    let cleared = OwnedBatch {
        subtrees: vec![OwnedSubtree::Clear(sub(UNDER[0]))],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &cleared).expect("the Merk is cleared");
    let held = held_by(&tx);
    let refused = tx.txn.evaluate(sub(TOP), &mut Rules(&db.counts.rule_calls));
    match refused {
        Err(TxnError::Hash(HashError::HostBehind { host })) => {
            let cell = (tx.txn.node(sub(TOP), &host_of(0), handle_of)).expect("a read");
            assert_eq!(Some(host), cell.map(|handle| handle.address()));
        }
        other => panic!("the host points at no root: {other:?}"),
    }
    assert_eq!(calls(), 0);
    assert_eq!(held_by(&tx), held);
    assert_eq!(stale_in(&world, &tx, TOP), stale);
    match db.commit(&versions, &tx.txn) {
        Err(TxnError::Batch(BatchError::ClearedAlone { subtree })) => {
            assert_eq!(subtree, sub(UNDER[0]))
        }
        other => panic!("the commit is refused too: {other:?}"),
    }
    // The cleared Merk has no node, and the other is not behind anything.
    assert_eq!(
        tx.txn
            .evaluate(sub(UNDER[0]), &mut Rules(&db.counts.rule_calls))
            .expect("no node"),
        None
    );
    assert_eq!(
        evaluate_merged(&world, &tx, UNDER[1]),
        expected(&world, &tx, UNDER[1])
    );

    // The host is written again, and names its Merk, which has no root:
    // the call makes its value hash from a Merk with no root.
    let mut top = left(&world, &tx, TOP, |items| {
        items.insert(host_of(0), b"tree, emptied".to_vec());
    });
    top.hosted.insert(host_of(0), vec![sub(UNDER[0])]);
    world
        .apply(&tx, &batch(vec![top]))
        .expect("the host is written");
    let scope = tx.txn.scope().expect("a scope");
    let mut rules = Hosting {
        rules: Rules(&db.counts.rule_calls),
        hosted: BTreeMap::new(),
    };
    let hash = evaluate_with(&world, &tx, &scope, TOP, &[TOP], &mut rules);
    assert_eq!(
        hash.expect("the hashes are made"),
        expected(&world, &tx, TOP)
    );
    scope.merge();
    assert_eq!(rules.hosted.get(&host_of(0)), Some(&vec![None]));
    assert_eq!(
        rules.hosted.get(&host_of(1)),
        Some(&vec![expected(&world, &tx, UNDER[1])])
    );
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

/// The shortest value that the store of these tests puts out of line.
const LONG: usize = lignin_tree::value::InlinePolicy::DEFAULT_THRESHOLD + 1;

/// A value hash is made from the value wherever it is: in its cell, with
/// the transaction that wrote it, and where a node keeps the value that
/// the committed state has out of line.
#[test]
fn a_value_hash_is_made_from_a_value_that_is_not_in_its_cell() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(120);
    let made = world.edit_outside(
        1,
        |items| {
            for index in 0..30 {
                let len = match index % 3 {
                    0 => LONG + 40 * index,
                    _ => 50,
                };
                items.insert(key(index), value(&mut rng, len));
            }
        },
        &[],
    );
    assert!(committed(made));
    let calls = || db.counts.rule_calls.load(Ordering::Relaxed);
    let evaluated = || db.counts.stale_nodes.load(Ordering::Relaxed);

    let tx = world.begin();
    let merk = left(&world, &tx, 1, |items| {
        // New values out of line and in their cells, and a node less:
        // the nodes above them are written with their values kept.
        items.insert(key(100), value(&mut rng, LONG * 3));
        items.insert(key(101), value(&mut rng, 20));
        items.insert(key(6), value(&mut rng, LONG + 7));
        items.remove(&key(14));
    });
    let kept_long = merk.nodes.iter().filter(|(key, node, stated)| {
        !stated && node.value.len() >= LONG && merk.stale.contains_key(key)
    });
    assert!(
        kept_long.count() > 0,
        "a node keeps a committed value that is out of line"
    );
    let to_make = calls_for(&merk);
    world.apply(&tx, &batch(vec![merk])).expect("the change");
    let root_hash = expected(&world, &tx, 1);
    let before = calls();
    assert_eq!(evaluate_merged(&world, &tx, 1), root_hash);
    assert_eq!(calls() - before, to_make + 1);
    world.check(&tx);

    // Frames that are made again read the kept values where they are.
    world.edit_outside(2, |items| drop(items.insert(key(1), vec![1; 30])), &[&tx]);
    world.check(&tx);
    assert_eq!(tx.txn.stats().stale_nodes, 0);
    let before = (calls(), evaluated());
    assert!(committed(world.commit(tx, &[])));
    assert_eq!((calls(), evaluated()), before);
    world.check_committed();
}

/// The small Merk that the tests enter under the element `index` of
/// [`PARENT`](super::trees::PARENT).
fn little(index: usize) -> u8 {
    ENTERED + 1 + index as u8
}

/// A batch that changes the Merks [`little`] of `changed` and the
/// elements that host them, which name their roots again
/// (`trees::hosted_batch`, of Merks that are entered under their
/// elements). With `enter` it enters each under its element.
fn little_batch<'s>(
    world: &World<'s>,
    tx: &Tx<'s>,
    changed: &[usize],
    enter: bool,
    edit: &mut dyn FnMut(usize, &mut Items),
    more: &mut dyn FnMut(&mut Items),
) -> OwnedBatch {
    use super::trees::{element, PARENT};

    let mut merks: Vec<OwnedMerk> = Vec::new();
    for index in changed {
        merks.push(world.merk(tx, little(*index), |items| edit(*index, items)));
    }
    let roots: Vec<Option<Vec<u8>>> = (changed.iter().zip(&merks))
        .map(|(index, merk)| match &merk.root {
            Some(root) => root.clone(),
            None => model::records(&items(world, tx, little(*index))).0,
        })
        .collect();
    let mut parent = world.merk(tx, PARENT, |items| {
        for (index, root) in changed.iter().zip(&roots) {
            let mut value = b"tree:".to_vec();
            value.extend_from_slice(root.as_deref().unwrap_or(b"-"));
            value.extend_from_slice(&[merks.len() as u8; 30]);
            items.insert(element(*index), value);
        }
        more(items);
    });
    for index in changed {
        (parent.hosted).insert(element(*index), vec![sub(little(*index))]);
    }
    let host = |index: &usize| (sub(PARENT), element(*index), 0);
    let entered = |index: &usize| OwnedSubtree::Enter(sub(little(*index)), Some(host(index)));
    let records = World::root_record(&parent).into_iter().collect();
    merks.push(parent);
    OwnedBatch {
        merks,
        records,
        subtrees: changed.iter().filter(|_| enter).map(entered).collect(),
    }
}

/// The marks of cells that a later write carried to another page are
/// where the cells are, and the call finds them there: thirty small
/// Merks whose nodes have stale hashes, under elements with stale value
/// hashes, in a Merk that then grows until its pages are cut, so that
/// hosts go to other pages and take their Merks along. One call for the
/// Merk above makes the hashes of all of them, each Merk kept as an
/// operation of its own that leaves its root where its host points.
#[test]
fn the_call_finds_the_stale_hashes_of_cells_that_were_carried() {
    use super::trees::{element, PARENT};

    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(121);
    let calls = || db.counts.rule_calls.load(Ordering::Relaxed);
    let evaluated = || db.counts.stale_nodes.load(Ordering::Relaxed);

    let tx = world.begin();
    let all: Vec<usize> = (0..30).collect();
    let mut made = little_batch(
        &world,
        &tx,
        &all,
        true,
        &mut |index, items| {
            for node in 0..1 + index % 3 {
                items.insert(vec![b'n', node as u8], value(&mut rng, 40));
            }
        },
        &mut |_| {},
    );
    made.merks.iter_mut().for_each(leave);
    let stale: usize = made.merks.iter().map(|merk| merk.nodes.len()).sum();
    world.apply(&tx, &made).expect("the Merks are made");
    assert_eq!(tx.txn.stats().stale_nodes, stale);
    let page_of = |tx: &Tx<'_>, index: usize| {
        let root = vec![b'n', 0];
        let handle = tx.txn.node(sub(little(index)), &root, handle_of);
        handle.expect("a read").expect("a cell").page()
    };
    let before: Vec<_> = all.iter().map(|index| page_of(&tx, *index)).collect();

    // The parent grows, with the hashes of what is written left again,
    // and its pages are cut. No batch that makes it grow names one of
    // the small Merks.
    let mut carried = 0;
    for round in 0..6 {
        let mut grown = little_batch(&world, &tx, &[], false, &mut |_, _| {}, &mut |items| {
            for index in 0..40 {
                let mut key = element(rng.below(30));
                key.extend_from_slice(format!("-{round}-{index}").as_bytes());
                items.insert(key, value(&mut rng, 150));
            }
        });
        assert_eq!(grown.merks.len(), 1, "the parent alone is written");
        grown.merks.iter_mut().for_each(leave);
        carried += world
            .apply(&tx, &grown)
            .expect("the parent grows")
            .carried
            .len();
    }
    let moved = (all.iter()).filter(|index| page_of(&tx, **index) != before[**index]);
    let moved = moved.count();
    assert!(moved > 0 && carried >= moved, "{moved} of {carried} cells");
    let stale = tx.txn.stats().stale_nodes;
    assert!(stale > 100, "{stale} nodes have a stale hash");

    let hashes: Vec<Option<Hash>> = (all.iter())
        .map(|index| expected(&world, &tx, little(*index)))
        .collect();
    let root_hash = expected(&world, &tx, PARENT);
    let reached: Vec<u8> = std::iter::once(PARENT)
        .chain(all.iter().map(|index| little(*index)))
        .collect();
    let scope = tx.txn.scope().expect("a scope");
    held::compared();
    let hash = evaluate(&world, &tx, &scope, PARENT, &reached);
    assert_eq!(hash.expect("the hashes are made"), root_hash);
    scope.merge();
    // Each cell is where its key leads to, in the Merk it is kept under.
    assert_eq!(held::compared().evaluated, stale as u64);
    assert_eq!(tx.txn.stats().stale_nodes, 0);
    world.check(&tx);
    for (index, hash) in all.iter().zip(&hashes) {
        let before = calls();
        assert_eq!(evaluate_merged(&world, &tx, little(*index)), *hash);
        assert_eq!(calls() - before, 1);
    }

    // Frames that are made again lay each of those operations out, with
    // the roots of the small Merks where their hosts point.
    let rebuilds = tx.txn.stats().rebuilds;
    world.edit_outside(2, |items| drop(items.insert(key(1), vec![1; 30])), &[&tx]);
    world.check(&tx);
    let stats = tx.txn.stats();
    assert_eq!((stats.rebuilds, stats.stale_nodes), (rebuilds + 1, 0));
    let before = (calls(), evaluated());
    assert!(committed(world.commit(tx, &[])));
    assert_eq!((calls(), evaluated()), before);
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
}

/// The call names the Merk that a host points into by the subtree
/// directory, as the Merk that is entered under the host: a store is
/// sound only where every hosted Merk is (`crate::verify`). A host whose
/// cell names the root of a Merk that is entered under no host, as the
/// stores of some tests have them, refuses the call as a host that is
/// behind. The commit follows the pointer as it is.
#[test]
fn a_host_that_points_into_a_merk_that_is_not_entered_under_it_refuses_the_call() {
    use super::trees::{element, hosted_batch, PARENT};

    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(124);
    let calls = || db.counts.rule_calls.load(Ordering::Relaxed);
    let evaluated = || db.counts.stale_nodes.load(Ordering::Relaxed);
    let tx = world.begin();
    let mut made = hosted_batch(
        &world,
        &tx,
        &[0, 1],
        &mut |_, items| drop(items.insert(b"n".to_vec(), value(&mut rng, 40))),
        &mut |_| {},
    );
    made.merks.iter_mut().for_each(leave);
    let stale: usize = made.merks.iter().map(|merk| merk.nodes.len()).sum();
    world.apply(&tx, &made).expect("the Merks are made");

    let held = held_by(&tx);
    let refused = tx
        .txn
        .evaluate(sub(PARENT), &mut Rules(&db.counts.rule_calls));
    match refused {
        Err(TxnError::Hash(HashError::HostBehind { host })) => {
            let cells = [0, 1].map(|index| {
                let read = tx.txn.node(sub(PARENT), &element(index), handle_of);
                read.expect("a read").map(|handle| handle.address())
            });
            assert!(cells.contains(&Some(host)), "{host:?} of {cells:?}");
        }
        other => panic!("no Merk is entered under the host: {other:?}"),
    }
    assert_eq!(calls(), 0);
    assert_eq!(held_by(&tx), held);
    assert_eq!(tx.txn.stats().stale_nodes, stale);
    let before = evaluated();
    assert!(committed(world.commit(tx, &[])));
    assert_eq!(evaluated() - before, stale as u64);
    world.check_committed();
}

/// A Merk that is cleared or dropped keeps its cells in their pages, and
/// the marks of their stale hashes with them (`lignin_tree::hash`): the
/// commit makes those hashes with the others, with a call before it and
/// with none. The call makes the hashes of the Merk as it is, and does
/// not come to a cell that the clear took.
#[test]
fn the_marks_of_what_a_clear_took_are_the_commits_with_a_call_and_with_none() {
    for call in [false, true] {
        let (db, versions) = Db::new(96);
        let mut world = World::new(&db, &versions);
        let mut rng = Rng::new(125);
        planted(&mut world, &mut rng);
        let evaluated = || db.counts.stale_nodes.load(Ordering::Relaxed);

        let tx = world.begin();
        let first = left(&world, &tx, 1, |items| {
            drop(items.insert(key(901), value(&mut rng, 40)))
        });
        let taken = first.nodes.len();
        world.apply(&tx, &batch(vec![first])).expect("a change");
        let cleared = OwnedBatch {
            subtrees: vec![OwnedSubtree::Clear(sub(1))],
            ..OwnedBatch::default()
        };
        world.apply(&tx, &cleared).expect("the Merk is cleared");
        assert_eq!(tx.txn.stats().stale_nodes, taken);
        let second = left(&world, &tx, 1, |items| {
            drop(items.insert(key(2), value(&mut rng, 40)))
        });
        let live = second.nodes.len();
        world.apply(&tx, &batch(vec![second])).expect("a node");
        assert_eq!(tx.txn.stats().stale_nodes, taken + live);
        if call {
            let root_hash = expected(&world, &tx, 1);
            assert_eq!(evaluate_merged(&world, &tx, 1), root_hash);
            assert_eq!(stale_in(&world, &tx, 1), BTreeSet::new());
            assert_eq!(tx.txn.stats().stale_nodes, taken);
        }
        let before = evaluated();
        assert!(committed(world.commit(tx, &[])));
        let left_to_the_commit = match call {
            true => taken,
            false => taken + live,
        };
        assert_eq!(evaluated() - before, left_to_the_commit as u64, "{call}");
        world.check_committed();
    }
}

/// The call makes every stale hash of the Merk it is asked for, also one
/// that no stale hash above it leads to, which the evaluation of the tree
/// layer does not come to from the root. A writer states the hashes of
/// the root with stale hashes left below it: the call finds those by the
/// writes that left them, a node that is read afterwards has its hashes,
/// and the commit makes none.
#[test]
fn the_call_makes_a_stale_hash_that_no_stale_hash_leads_to() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(122);
    planted(&mut world, &mut rng);
    let calls = || db.counts.rule_calls.load(Ordering::Relaxed);
    let evaluated = || db.counts.stale_nodes.load(Ordering::Relaxed);

    let tx = world.begin();
    let merk = changed(&world, &tx, &mut rng, 1, 0);
    world.apply(&tx, &batch(vec![merk])).expect("the change");
    // A second writer makes every hash of what it writes, the root among
    // it, and states for a child the hash that the rules make of it.
    let above = world.merk(&tx, 1, |items| drop(items.insert(key(900), vec![5; 40])));
    let root = (above.root.clone().flatten())
        .or_else(|| model::records(&items(&world, &tx, 1)).0)
        .expect("a root");
    assert!(above.nodes.iter().any(|(key, ..)| *key == root));
    world
        .apply(&tx, &batch(vec![above]))
        .expect("the root is written");
    let stale = stale_in(&world, &tx, 1);
    assert!(stale.len() > 3 && !stale.contains(&root), "{stale:?}");
    assert_eq!(tx.txn.stats().stale_nodes, stale.len());
    let (stale_root, ..) = shown(&tx, 1, &root);
    assert_eq!(
        stale_root,
        Stale::NONE,
        "no stale hash leads down from the root"
    );

    // A savepoint, and a third writer that leaves a hash of one node
    // more: the rollback takes that one from the list of what is left.
    let model_writes = tx.model.borrow().writes.len();
    tx.txn.set_savepoint().expect("no scope is open");
    let more = left(&world, &tx, 1, |items| {
        drop(items.insert(key(901), vec![6; 40]))
    });
    world.apply(&tx, &batch(vec![more])).expect("one more");
    assert!(stale_in(&world, &tx, 1).len() > stale.len());
    tx.txn.rollback_to_savepoint().expect("a savepoint is set");
    tx.model.borrow_mut().writes.truncate(model_writes);
    assert_eq!(stale_in(&world, &tx, 1), stale);

    // The call makes each of them once, with one hash more for the
    // root's own, which no cell stores.
    let fields = |key: &Vec<u8>| {
        let (stale, ..) = shown(&tx, 1, key);
        Stale::ALL
            .fields()
            .filter(|field| stale.has(*field))
            .count() as u64
    };
    let to_make: u64 = stale.iter().map(fields).sum();
    assert!(to_make > 2 * stale.len() as u64, "{to_make}");
    let root_hash = expected(&world, &tx, 1);
    let (before, made) = (calls(), evaluated());
    held::compared();
    assert_eq!(evaluate_merged(&world, &tx, 1), root_hash);
    assert_eq!(calls() - before, to_make + 1);
    assert_eq!(held::compared().evaluated, stale.len() as u64);
    assert_eq!(stale_in(&world, &tx, 1), BTreeSet::new());
    assert_eq!(tx.txn.stats().stale_nodes, 0);
    world.check(&tx);

    // A second call makes the root's own hash, and the commit makes none.
    let before = calls();
    assert_eq!(evaluate_merged(&world, &tx, 1), root_hash);
    assert_eq!(calls() - before, 1);
    let before = (calls(), evaluated());
    assert!(committed(world.commit(tx, &[])));
    assert_eq!((calls(), evaluated()), before);
    assert_eq!(evaluated(), made, "no node was left for a commit");
    world.check_committed();
}
