//! Unlinked records: a node of a batch that the tree of its Merk does not
//! reach is kept as a record of its key, read by key and under a cursor
//! as an engine with one record for a key reads it, and a Merk that holds
//! none is not charged for them.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    model::{self, records, Committed, LinkRec, NodeRec, OwnedBatch, OwnedMerk, OwnedSubtree, Val},
    store::{Db, ENTERED},
    *,
};
use crate::txn::unlinked::probe::{lookups, reads};

const MERK: u8 = 1;

fn subtree() -> Subtree {
    model::subtree(MERK)
}

fn unit() -> Unit {
    Unit::data(subtree())
}

fn aux() -> Unit {
    model::unit(Column::Aux, 0)
}

fn key(n: u8) -> Vec<u8> {
    vec![b'k', b'a' + n]
}

/// The records of the Merk as `tx` reads them, by key: its nodes and its
/// unlinked records alike.
fn view(world: &World<'_>, tx: &Tx<'_>) -> BTreeMap<Vec<u8>, Val> {
    tx.model.borrow().view(&world.model.records, unit())
}

fn node_of(view: &BTreeMap<Vec<u8>, Val>, key: &[u8]) -> NodeRec {
    match view.get(key) {
        Some(Val::Node(node)) => node.clone(),
        other => panic!("{key:?} holds {other:?}"),
    }
}

/// The change set that takes the tree of the Merk, whose nodes are the
/// records of `tx`'s view but for `unlinked`, to the tree of the items
/// `edit` leaves, with its root stated.
fn change(
    world: &World<'_>,
    tx: &Tx<'_>,
    unlinked: &BTreeSet<Vec<u8>>,
    edit: impl FnOnce(&mut BTreeMap<Vec<u8>, Vec<u8>>),
) -> OwnedMerk {
    let mut tree = view(world, tx);
    tree.retain(|key, _| !unlinked.contains(key));
    let mut items = Committed::items(&tree);
    edit(&mut items);
    let mut merk = OwnedMerk::change(subtree(), &tree, &items);
    merk.root = Some(records(&items).0);
    merk
}

fn batch_of(merk: OwnedMerk) -> OwnedBatch {
    OwnedBatch {
        merks: vec![merk],
        ..OwnedBatch::default()
    }
}

/// Which of `keys` the transaction shows as an unlinked record, as a node
/// of the tree (`Some(false)`) or not at all.
fn shown(tx: &Tx<'_>, keys: &[&[u8]]) -> Vec<Option<bool>> {
    let read = |key: &&[u8]| {
        (tx.txn).node(subtree(), key, |view| {
            view.map(|view| {
                assert_eq!(view.handle().is_none(), view.is_unlinked());
                view.is_unlinked()
            })
        })
    };
    keys.iter()
        .map(|key| read(key).expect("the node is read"))
        .collect()
}

/// The keys of the flat records that the data column holds under the Merk
/// as a transaction with no write reads them.
fn records_beside_the_tree(world: &World<'_>) -> Vec<Vec<u8>> {
    let reader = world.begin();
    let mut cursor = reader.txn.cursor(unit());
    scan(&mut cursor, false)
        .into_iter()
        .map(|(key, _)| key)
        .collect()
}

/// Checks the committed state: every read against the model, and that
/// the tree pages hold the nodes of the model but for `unlinked`, with no
/// cell that nothing reaches.
fn check_committed(world: &World<'_>, unlinked: &BTreeSet<Vec<u8>>) {
    let reader = world.begin();
    world.check(&reader);
    let keys = world.db.check_trees(world.versions, world.latest());
    let mut tree: Vec<Vec<u8>> = (world.model.records.iter())
        .filter(|((at, key), value)| {
            *at == unit() && matches!(value, Val::Node(_)) && !unlinked.contains(key)
        })
        .map(|((_, key), _)| key.clone())
        .collect();
    tree.sort();
    assert_eq!(keys.get(&subtree()), Some(&tree), "the nodes of the tree");
    let beside: Vec<Vec<u8>> = unlinked.iter().cloned().collect();
    assert_eq!(records_beside_the_tree(world), beside);
}

/// A storage batch keeps a put of a node over a later delete of it inside
/// one operation. Such a batch reaches the transaction as a change set
/// that writes a node its root does not reach. The transaction keeps the
/// node as a record of its key: it is read by key and under a cursor in
/// every state of a transaction, survives a commit, has no value for a
/// write to keep, gives way to a node that is linked under its key, and
/// goes with a delete of its key and with a clear of its subtree.
#[test]
fn a_node_that_its_tree_does_not_reach_is_a_record_of_its_key() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    world.edit_outside(
        MERK,
        |items| {
            for n in 0..14 {
                items.insert(key(n), vec![n; 20 + usize::from(n) * 3]);
            }
        },
        &[],
    );
    world.check_committed();
    let (rewritten, kept, made) = (key(3), key(8), b"loose".to_vec());
    let stray: BTreeSet<Vec<u8>> = [&rewritten, &kept, &made].into_iter().cloned().collect();
    let names: Vec<&[u8]> = vec![&rewritten, &kept, &made, b"ka", b"absent"];

    // One operation whose first commit rewrote two nodes and wrote a new
    // one, and whose second removed all three: the puts stand, and the
    // root that is stated reaches none of them.
    let the_batch = |world: &World<'_>, tx: &Tx<'_>| {
        let before = view(world, tx);
        let mut merk = change(world, tx, &BTreeSet::new(), |items| {
            items.remove(&rewritten);
            items.remove(&kept);
            items.insert(b"kzz".to_vec(), b"new".to_vec());
        });
        assert_eq!(merk.removed, [rewritten.clone(), kept.clone()]);
        merk.removed.clear();
        let linked = merk.nodes.len();
        let stale = NodeRec {
            value: b"rewritten before it was removed".to_vec(),
            ..node_of(&before, &rewritten)
        };
        // A record names its children by key, and they need not be there.
        let loose = NodeRec {
            value: vec![9; 300],
            feature: vec![3, 1],
            value_hash: [1; 32],
            kv_hash: [2; 32],
            left: Some(LinkRec {
                key: b"nowhere".to_vec(),
                hash: [3; 32],
                heights: (1, 2),
                aggregate: vec![4, 5],
            }),
            right: None,
        };
        merk.nodes.push((rewritten.clone(), stale, true));
        merk.nodes
            .push((kept.clone(), node_of(&before, &kept), false));
        merk.nodes.push((made.clone(), loose, true));
        (batch_of(merk), linked)
    };

    let tx = world.begin();
    let mark = tx.txn.mark();
    let (batch, linked) = the_batch(&world, &tx);
    let applied = world.apply(&tx, &batch).expect("the batch is applied");
    // The nodes of the tree have their cells, and the three have none.
    assert_eq!(applied.nodes.len(), linked);
    assert_eq!(applied.unlinked, [linked, linked + 1, linked + 2]);
    let as_left = [Some(true), Some(true), Some(true), Some(false), None];
    assert_eq!(shown(&tx, &names), as_left);
    world.check(&tx);

    // A rollback to a mark takes them out, and the same batch brings them
    // back.
    assert!(tx.txn.rewind(mark));
    tx.forget();
    world.check(&tx);
    assert_eq!(
        shown(&tx, &names),
        [Some(false), Some(false), None, Some(false), None]
    );
    let (batch, _) = the_batch(&world, &tx);
    world.apply(&tx, &batch).expect("the batch is applied");
    world.check(&tx);

    // A commit of another transaction that meets none of its keys has the
    // frames made again over the new version (WT7a).
    let rebuilds = tx.txn.stats().rebuilds;
    world.edit_outside(
        2,
        |items| drop(items.insert(b"beside".to_vec(), b"v".to_vec())),
        &[&tx],
    );
    world.check(&tx);
    assert!(tx.txn.stats().rebuilds > rebuilds);
    assert_eq!(shown(&tx, &names), as_left);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    check_committed(&world, &stray);
    let reader = world.begin();
    assert_eq!(shown(&reader, &names), as_left);

    // One change set links nodes under the keys of two records and deletes
    // the third by its key. A record is no node: a node that is linked
    // under its key states its value, and one that keeps a value is
    // refused.
    let relink = |world: &World<'_>, tx: &Tx<'_>, keeps: bool| {
        let mut relink = change(world, tx, &stray, |items| {
            let value = node_of(&view(world, tx), &kept).value;
            items.insert(kept.clone(), value);
            items.insert(made.clone(), b"linked now".to_vec());
        });
        for node in &mut relink.nodes {
            node.2 &= !(keeps && node.0 == kept);
        }
        relink.removed.push(rewritten.clone());
        batch_of(relink)
    };
    let no_value = |applied: Result<Applied, TxnError>| {
        assert!(
            matches!(&applied, Err(TxnError::Batch(BatchError::NoValue { subtree: at })) if *at == subtree()),
            "{applied:?}"
        );
    };

    // A transaction that a commit has overlapped serves its records by
    // key (WT7b), and reads the committed records beside them as before.
    let overlapped = world.begin();
    world.write(&overlapped, vec![put(aux(), b"mine", b"1")]);
    let outside = OwnedBatch {
        records: vec![put(aux(), b"mine", b"2")],
        ..OwnedBatch::default()
    };
    world.write_outside(&outside, &[&overlapped]);
    world.check(&overlapped);
    assert!(overlapped.txn.stats().overlapped > 0);
    assert_eq!(shown(&overlapped, &names), as_left);
    no_value(world.apply(&overlapped, &relink(&world, &overlapped, true)));
    world.check(&overlapped);
    let taken = world.apply(&overlapped, &relink(&world, &overlapped, false));
    taken.expect("the batch is taken by key");
    world.check(&overlapped);
    assert!(matches!(world.commit(overlapped, &[]), Outcome::Conflict));
    check_committed(&world, &stray);

    let tx = world.begin();
    no_value(world.apply(&tx, &relink(&world, &tx, true)));
    world.check(&tx);
    assert_eq!(shown(&tx, &names), as_left);
    let applied = world.apply(&tx, &relink(&world, &tx, false));
    assert!(applied.expect("the batch is applied").unlinked.is_empty());
    world.check(&tx);
    let linked = [None, Some(false), Some(false), Some(false), None];
    assert_eq!(shown(&tx, &names), linked);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    check_committed(&world, &BTreeSet::new());

    // Under a root that is kept, a node with a cell stays in its tree, and
    // a new node that no node links is a record. A delete of its key takes
    // it, with no node of the tree touched.
    let tx = world.begin();
    let before = view(&world, &tx);
    let alone = NodeRec {
        value: b"alone".to_vec(),
        left: None,
        right: None,
        ..node_of(&before, b"ka")
    };
    let rewrite = OwnedMerk {
        nodes: vec![
            (b"ka".to_vec(), node_of(&before, b"ka"), false),
            (b"alone".to_vec(), alone, true),
        ],
        ..change(&world, &tx, &BTreeSet::new(), |_| ())
    };
    let rewrite = OwnedMerk {
        root: None,
        ..rewrite
    };
    let applied = (world.apply(&tx, &batch_of(rewrite))).expect("the batch is applied");
    assert_eq!((applied.nodes.len(), &applied.unlinked[..]), (1, &[1][..]));
    world.check(&tx);
    assert_eq!(shown(&tx, &[b"alone", b"ka"]), [Some(true), Some(false)]);
    let removal = OwnedMerk {
        nodes: Vec::new(),
        removed: vec![b"alone".to_vec()],
        ..change(&world, &tx, &BTreeSet::new(), |_| ())
    };
    let removal = OwnedMerk {
        root: None,
        ..removal
    };
    // While the Merk holds the record, a read that finds no node reads
    // the flat keyspace for one under its key.
    let read = reads();
    assert_eq!(shown(&tx, &[b"absent"]), [None]);
    assert!(reads() > read);
    (world.apply(&tx, &batch_of(removal))).expect("the batch is applied");
    // The Merk holds no record any more. It is asked once more whether it
    // does, and a read that finds no node reads the tree alone after that.
    let (asked, read) = (lookups(), reads());
    for _ in 0..3 {
        assert_eq!(shown(&tx, &[b"alone", b"absent"]), [None, None]);
    }
    assert_eq!((lookups(), reads()), (asked + 1, read));
    world.check(&tx);
    assert_eq!(shown(&tx, &[b"alone"]), [None]);

    // A record follows its subtree: a clear takes it with the nodes, and
    // the Merk is written again with neither.
    let alone = NodeRec {
        value: b"cleared with its subtree".to_vec(),
        left: None,
        right: None,
        ..node_of(&before, b"ka")
    };
    let again = OwnedMerk {
        nodes: vec![(b"alone".to_vec(), alone, true)],
        root: None,
        ..change(&world, &tx, &BTreeSet::new(), |_| ())
    };
    (world.apply(&tx, &batch_of(again))).expect("the batch is applied");
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    let only: BTreeSet<Vec<u8>> = [b"alone".to_vec()].into();
    check_committed(&world, &only);
    let tx = world.begin();
    let clear = OwnedBatch {
        subtrees: vec![OwnedSubtree::Clear(subtree())],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &clear).expect("the subtree is cleared");
    world.check(&tx);
    assert_eq!(shown(&tx, &[b"alone", b"ka"]), [None, None]);
    world.edit(&tx, MERK, |items| {
        drop(items.insert(b"alone".to_vec(), b"a node".to_vec()))
    });
    world.check(&tx);
    assert_eq!(shown(&tx, &[b"alone"]), [Some(false)]);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    check_committed(&world, &BTreeSet::new());
    assert_eq!(db.private_ids(), 0);
}

/// A node that the transaction left out of its tree is its own node
/// still: a later write of it that keeps its value keeps the value of the
/// record the transaction left, whether that write leaves the node out of
/// its tree again or links it. One of the two nodes took its value from
/// the cell it had, and the other had it stated. It holds where every
/// write is laid out as it is made, where the write that left the records
/// was kept in the write set beside another scope and laid out when it
/// merged, where the later write is, and in frames that were made again.
#[test]
fn a_node_the_transaction_left_unlinked_keeps_its_value() {
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Beside {
        Never,
        Leaving,
        Again,
    }
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    world.edit_outside(
        MERK,
        |items| {
            for n in 0..14 {
                items.insert(key(n), vec![n; 20 + usize::from(n) * 3]);
            }
        },
        &[],
    );
    let (from_cell, stated) = (key(3), key(8));
    let stray: BTreeSet<Vec<u8>> = [&from_cell, &stated].into_iter().cloned().collect();
    let names: Vec<&[u8]> = vec![&from_cell, &stated];
    // Applies `batch` while another scope of the transaction is open, so
    // that its writes are kept in the write set, or with none.
    fn apply<'s>(world: &World<'s>, tx: &Tx<'s>, beside: bool, batch: &OwnedBatch) -> Applied {
        let idle = beside.then(|| tx.txn.scope().expect("a scope"));
        let applied = world.apply(tx, batch);
        drop(idle);
        applied.expect("the batch is applied")
    }

    for (round, beside) in [Beside::Never, Beside::Leaving, Beside::Again]
        .into_iter()
        .enumerate()
    {
        let tx = world.begin();
        let before = view(&world, &tx);
        let mut leaving = change(&world, &tx, &BTreeSet::new(), |items| {
            items.remove(&from_cell);
            items.remove(&stated);
        });
        leaving.removed.clear();
        let linked = leaving.nodes.len();
        leaving
            .nodes
            .push((from_cell.clone(), node_of(&before, &from_cell), false));
        let other = NodeRec {
            value: format!("stated by the transaction in round {round}").into_bytes(),
            ..node_of(&before, &stated)
        };
        leaving.nodes.push((stated.clone(), other.clone(), true));
        let applied = apply(&world, &tx, beside == Beside::Leaving, &batch_of(leaving));
        match beside {
            Beside::Leaving => assert_eq!(applied, Applied::default()),
            _ => assert_eq!(applied.unlinked, [linked, linked + 1]),
        }
        world.check(&tx);
        assert_eq!(shown(&tx, &names), [Some(true), Some(true)]);

        // Both are written again, out of the tree and with other hashes,
        // and keep their values.
        let now = view(&world, &tx);
        let rehashed = |key: &Vec<u8>| {
            let node = NodeRec {
                kv_hash: [0x77; 32],
                ..node_of(&now, key)
            };
            (key.clone(), node, false)
        };
        let again = OwnedMerk {
            nodes: vec![rehashed(&from_cell), rehashed(&stated)],
            ..change(&world, &tx, &stray, |_| ())
        };
        apply(&world, &tx, beside == Beside::Again, &batch_of(again));
        world.check(&tx);
        assert_eq!(shown(&tx, &names), [Some(true), Some(true)]);
        let kept = view(&world, &tx);
        assert_eq!(
            node_of(&kept, &from_cell).value,
            node_of(&before, &from_cell).value
        );
        assert_eq!(node_of(&kept, &stated).value, other.value);
        assert_eq!(node_of(&kept, &stated).kv_hash, [0x77; 32]);

        // A commit of another transaction that meets none of its keys has
        // the frames made again (WT7a), with the same records.
        let rebuilds = tx.txn.stats().rebuilds;
        world.edit_outside(
            2,
            |items| drop(items.insert(vec![b'b', round as u8], b"v".to_vec())),
            &[&tx],
        );
        world.check(&tx);
        assert!(tx.txn.stats().rebuilds > rebuilds);

        // And both are linked again with their values kept.
        let mut relink = change(&world, &tx, &stray, |items| {
            for key in [&from_cell, &stated] {
                items.insert(key.clone(), node_of(&kept, key).value);
            }
        });
        for node in &mut relink.nodes {
            node.2 &= !stray.contains(&node.0);
        }
        assert!(relink.nodes.iter().filter(|node| !node.2).count() >= 2);
        let applied = apply(&world, &tx, false, &batch_of(relink));
        assert!(applied.unlinked.is_empty());
        world.check(&tx);
        assert_eq!(shown(&tx, &names), [Some(false), Some(false)]);
        assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
        check_committed(&world, &BTreeSet::new());
        let reader = world.begin();
        let committed = view(&world, &reader);
        assert_eq!(node_of(&committed, &stated).value, other.value);
    }
    assert_eq!(db.private_ids(), 0);
}

/// An enter of a subtree that is absent, where it finds the subtree
/// entered, leaves the subtree what it holds: the frames go on reading its
/// unlinked records, the committed one and the one the transaction left
/// itself, and a later write keeps the value of the transaction's own.
#[test]
fn an_enter_that_finds_its_subtree_entered_keeps_its_unlinked_records() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    world.edit_outside(
        MERK,
        |items| {
            for n in 0..6 {
                items.insert(key(n), vec![n; 24]);
            }
        },
        &[],
    );
    // A new node under `name` that no node links, under the root as it is.
    let alone = |world: &World<'_>, tx: &Tx<'_>, name: &[u8], stray: &BTreeSet<Vec<u8>>| {
        let node = NodeRec {
            value: name.to_vec(),
            left: None,
            right: None,
            ..node_of(&view(world, tx), b"ka")
        };
        batch_of(OwnedMerk {
            nodes: vec![(name.to_vec(), node, true)],
            root: None,
            ..change(world, tx, stray, |_| ())
        })
    };
    let mut stray = BTreeSet::new();
    let tx = world.begin();
    let leave = alone(&world, &tx, b"alone", &stray);
    world.apply(&tx, &leave).expect("the batch is applied");
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    stray.insert(b"alone".to_vec());
    check_committed(&world, &stray);

    // The frames of a transaction know that the Merk holds a record, and
    // the transaction leaves one of its own.
    let tx = world.begin();
    let leave = alone(&world, &tx, b"mine", &stray);
    world.apply(&tx, &leave).expect("the batch is applied");
    stray.insert(b"mine".to_vec());
    let names: [&[u8]; 3] = [b"alone", b"mine", b"absent"];
    let held = [Some(true), Some(true), None];
    assert_eq!(shown(&tx, &names), held);
    let found = OwnedBatch {
        subtrees: vec![OwnedSubtree::EnterIfAbsent(subtree(), None)],
        records: vec![put(aux(), b"beside", b"1")],
        ..OwnedBatch::default()
    };
    world
        .apply(&tx, &found)
        .expect("the subtree is found entered");
    world.check(&tx);
    assert_eq!(shown(&tx, &names), held);
    // The record the transaction left has its value for a write to keep.
    let rehashed = NodeRec {
        kv_hash: [0x55; 32],
        ..node_of(&view(&world, &tx), b"mine")
    };
    let again = OwnedMerk {
        nodes: vec![(b"mine".to_vec(), rehashed, false)],
        root: None,
        ..change(&world, &tx, &stray, |_| ())
    };
    let applied = world.apply(&tx, &batch_of(again));
    assert_eq!(applied.expect("the value is kept").unlinked, [0]);
    world.check(&tx);
    assert_eq!(shown(&tx, &names), held);
    let kept = node_of(&view(&world, &tx), b"mine");
    assert_eq!((&kept.value[..], kept.kv_hash), (&b"mine"[..], [0x55; 32]));
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    check_committed(&world, &stray);
    assert_eq!(db.private_ids(), 0);
}

/// One batch takes every node of a Merk out of its tree: it writes each
/// node with its value kept and states that the Merk has no root. The
/// tree is then empty, with no cell left in its pages, and every node is
/// a record of its key, read by key and in key order as before.
#[test]
fn a_batch_that_states_no_root_leaves_every_node_it_writes_a_record() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let all: BTreeSet<Vec<u8>> = (0..14).map(key).collect();
    world.edit_outside(
        MERK,
        |items| {
            for n in 0..14 {
                items.insert(key(n), vec![n; 20 + usize::from(n) * 3]);
            }
        },
        &[],
    );
    let tx = world.begin();
    let before = view(&world, &tx);
    let kept = |key: &Vec<u8>| (key.clone(), node_of(&before, key), false);
    let whole = OwnedMerk {
        nodes: all.iter().map(kept).collect(),
        root: Some(None),
        ..change(&world, &tx, &BTreeSet::new(), |_| ())
    };
    let applied = (world.apply(&tx, &batch_of(whole))).expect("the batch is applied");
    assert!(applied.nodes.is_empty());
    assert_eq!(applied.unlinked, (0..all.len()).collect::<Vec<_>>());
    world.check(&tx);
    let names: Vec<&[u8]> = all.iter().map(Vec::as_slice).collect();
    assert_eq!(shown(&tx, &names), vec![Some(true); all.len()]);
    assert_eq!(view(&world, &tx), before);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));

    let reader = world.begin();
    world.check(&reader);
    assert_eq!(shown(&reader, &names), vec![Some(true); all.len()]);
    let keys = world.db.check_trees(world.versions, world.latest());
    assert_eq!(keys.get(&subtree()), None, "the tree has no node");
    let beside: Vec<Vec<u8>> = all.iter().cloned().collect();
    assert_eq!(records_beside_the_tree(&world), beside);
    assert_eq!(db.private_ids(), 0);
}

/// A cursor over the nodes of a Merk that is pointed at another Merk
/// (`MerkCursor::retarget`) shows the unlinked records of the Merk it is
/// pointed at, among its nodes, and none of the Merk it was opened on.
/// Whether a Merk holds any is looked up once for the two cursors of a
/// transaction that reads one version, however often they are pointed at
/// it, and once for a cursor whose version the transaction has read
/// past.
#[test]
fn a_cursor_that_is_pointed_at_another_merk_shows_the_unlinked_records_of_that_merk() {
    fn shown(cursor: &mut MerkCursor<'_, '_>) -> Vec<(Vec<u8>, bool)> {
        let mut keys = Vec::new();
        cursor.seek_to_first().expect("a seek");
        while let Some(key) = cursor.key().map(<[u8]>::to_vec) {
            let loose = cursor.node(|view| view.expect("a node").is_unlinked());
            keys.push((key, loose.expect("the node is read")));
            cursor.next().expect("a step");
        }
        keys
    }
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    for name in [MERK, 2] {
        world.edit_outside(
            name,
            |items| {
                for n in 0..6 {
                    items.insert(key(n), vec![n; 30]);
                }
            },
            &[],
        );
    }
    let tx = world.begin();
    let loose = NodeRec {
        value: b"loose".to_vec(),
        left: None,
        right: None,
        ..node_of(&view(&world, &tx), &key(0))
    };
    let mut merk = change(&world, &tx, &BTreeSet::new(), |_| ());
    merk.nodes.push((b"kzz".to_vec(), loose, true));
    world
        .apply(&tx, &batch_of(merk))
        .expect("the batch is applied");
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));

    let reader = world.begin();
    let tree: Vec<(Vec<u8>, bool)> = (0..6).map(|n| (key(n), false)).collect();
    let mut with_record = tree.clone();
    with_record.push((b"kzz".to_vec(), true));
    let asked = lookups();
    let mut cursor = (reader.txn.merk_cursor(model::subtree(2))).expect("a cursor");
    assert_eq!(shown(&mut cursor), tree);
    assert_eq!(lookups(), asked + 1);
    cursor.retarget(subtree()).expect("a retarget");
    assert_eq!(shown(&mut cursor), with_record);
    assert_eq!(lookups(), asked + 2, "the Merk it is pointed at is asked");
    cursor.retarget(model::subtree(2)).expect("a retarget");
    assert_eq!(shown(&mut cursor), tree);
    let mut cursor = reader.txn.merk_cursor(subtree()).expect("a cursor");
    assert_eq!(shown(&mut cursor), with_record);
    cursor.retarget(model::subtree(2)).expect("a retarget");
    assert_eq!(shown(&mut cursor), tree);
    cursor.retarget(subtree()).expect("a retarget");
    assert_eq!(shown(&mut cursor), with_record);
    // A Merk with no node is asked as any other, and a prefix that the
    // version has no subtree under is not asked.
    cursor.retarget(model::subtree(9)).expect("a retarget");
    assert!(shown(&mut cursor).is_empty());
    assert_eq!(lookups(), asked + 3);
    cursor
        .retarget(model::subtree(ENTERED))
        .expect("a retarget");
    assert!(shown(&mut cursor).is_empty());
    cursor.retarget(model::subtree(9)).expect("a retarget");
    assert_eq!(lookups(), asked + 3, "each Merk was asked once");

    // A commit is published, and the transaction reads the version after
    // it. The cursor keeps its version, and what it found of each Merk: a
    // Merk that it is pointed at for the first time is looked up once,
    // however often it is pointed there afterwards.
    world.edit_outside(
        3,
        |items| drop(items.insert(b"later".to_vec(), b"v".to_vec())),
        &[&reader],
    );
    let later = (reader.txn).node(model::subtree(3), b"later", |view| view.is_some());
    assert!(later.expect("a read of the version after"));
    let asked = lookups();
    for _ in 0..3 {
        cursor.retarget(model::subtree(4)).expect("a retarget");
        assert!(shown(&mut cursor).is_empty());
        cursor.retarget(subtree()).expect("a retarget");
        assert_eq!(shown(&mut cursor), with_record);
        cursor.retarget(model::subtree(3)).expect("a retarget");
        assert!(shown(&mut cursor).is_empty(), "the version it pinned");
    }
    assert_eq!(lookups(), asked + 2, "the cursor keeps what it found");
}

/// A Merk that holds no unlinked record is asked once whether it holds
/// any, for the frames of a transaction and for each committed version it
/// reads beside them, and only by a read that finds no node, a cursor, or
/// a write that makes or deletes a node. Nothing else is read for them.
#[test]
fn a_merk_with_no_unlinked_record_is_asked_once_whether_it_has_one() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    for name in [MERK, 2] {
        world.edit_outside(
            name,
            |items| {
                for n in 0..12 {
                    items.insert(key(n), vec![n; 30]);
                }
            },
            &[],
        );
    }
    let read = |tx: &Tx<'_>, name: u8, key: &[u8]| {
        (tx.txn
            .node(model::subtree(name), key, |view| view.is_some()))
        .expect("a read")
    };
    let requests = || {
        let pool = db.pool.stats();
        pool.hits + pool.misses
    };

    // A transaction that has written nothing reads the committed pages.
    // A node that is there is all its read reads.
    let reader = world.begin();
    let asked = lookups();
    assert!(read(&reader, MERK, &key(4)));
    assert_eq!(lookups(), asked);
    // A key with no node asks, once for the version.
    let before = requests();
    assert!(!read(&reader, MERK, b"absent"));
    assert_eq!(lookups(), asked + 1);
    let first = requests() - before;
    let before = requests();
    assert!(!read(&reader, MERK, b"absent"));
    let again = requests() - before;
    let committed = (reader.txn).committed_node(subtree(), b"gone", |view| view.is_some());
    assert!(!committed.expect("a read") && !read(&reader, MERK, b"kzz"));
    assert_eq!(lookups(), asked + 1);
    // What the first of them read beyond the walk of the tree is the
    // lookup, and the later ones read the tree alone.
    assert!(first > again, "{first} requests, then {again}");
    let before = requests();
    assert!(!read(&reader, MERK, b"absent"));
    assert_eq!(requests() - before, again);
    // A cursor of that version takes what the transaction knows.
    let mut cursor = reader.txn.merk_cursor(subtree()).expect("a cursor");
    cursor.seek_to_first().expect("a seek");
    assert_eq!(cursor.key(), Some(&key(0)[..]));
    assert_eq!(lookups(), asked + 1);
    // Another Merk is asked for itself, by its first cursor.
    let mut cursor = (reader.txn.merk_cursor(model::subtree(2))).expect("a cursor");
    cursor.seek_to_last().expect("a seek");
    assert_eq!(lookups(), asked + 2);
    assert!(!read(&reader, 2, b"absent"));
    assert_eq!(lookups(), asked + 2);

    // A writer. A change that makes no node and deletes none asks nothing:
    // every node it writes has a cell that stays.
    let tx = world.begin();
    let asked = lookups();
    world.edit(&tx, MERK, |items| {
        drop(items.insert(key(5), b"another value".to_vec()))
    });
    assert_eq!(lookups(), asked);
    // The first change that makes a node asks, once for the frames.
    world.edit(&tx, MERK, |items| {
        drop(items.insert(b"new".to_vec(), b"v".to_vec()))
    });
    assert_eq!(lookups(), asked + 1);
    world.edit(&tx, MERK, |items| {
        items.remove(&key(2));
        items.insert(b"newer".to_vec(), b"v".to_vec());
    });
    assert!(!read(&tx, MERK, b"absent") && !read(&tx, MERK, &key(2)));
    assert_eq!(lookups(), asked + 1);
    world.check(&tx);
    // A commit outside has the frames made again, which ask again when
    // they first need to know.
    world.edit_outside(
        2,
        |items| drop(items.insert(b"beside".to_vec(), b"v".to_vec())),
        &[&tx],
    );
    let asked = lookups();
    assert!(read(&tx, MERK, b"newer"));
    assert_eq!(lookups(), asked + 1);
    assert!(!read(&tx, MERK, b"absent"));
    world.edit(&tx, MERK, |items| drop(items.remove(&key(7))));
    assert_eq!(lookups(), asked + 1);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
    world.check_committed();
}

/// A load takes the unlinked records under the keys of its nodes, as any
/// node that is new to its tree does, and leaves the others beside the
/// tree (`transactions.md` LD1). The Merk has no root and a record under
/// every key, and is handed over whole, with nodes under half of those
/// keys and under keys of its own.
#[test]
fn a_load_takes_the_unlinked_records_under_the_keys_of_its_nodes() {
    let (db, versions) = Db::new(128);
    let mut world = World::new(&db, &versions);
    let all: BTreeSet<Vec<u8>> = (0..14).map(key).collect();
    world.edit_outside(
        MERK,
        |items| {
            for n in 0..14 {
                items.insert(key(n), vec![n; 20 + usize::from(n) * 3]);
            }
        },
        &[],
    );
    let tx = world.begin();
    let before = view(&world, &tx);
    let kept = |key: &Vec<u8>| (key.clone(), node_of(&before, key), false);
    let no_root = OwnedMerk {
        nodes: all.iter().map(kept).collect(),
        root: Some(None),
        ..change(&world, &tx, &BTreeSet::new(), |_| ())
    };
    (world.apply(&tx, &batch_of(no_root))).expect("the batch is applied");
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));

    let mut items: BTreeMap<Vec<u8>, Vec<u8>> = (0..7)
        .map(|n| (key(n), vec![n + 100; 600 + usize::from(n) * 500]))
        .collect();
    items.extend((0..20).map(|n| (vec![b'm', n], vec![n; 30])));
    let tx = world.begin();
    let batch = batch_of(super::load::whole(MERK, &items));
    let scope = tx.txn.scope().expect("a scope");
    let first_write = world.latest().get();
    let applied = batch.loading(&[subtree()], |batch| scope.apply(batch));
    scope.merge();
    let applied = applied.expect("a load");
    world.note(&tx, &batch, first_write);
    assert!(applied.load.is_some());
    assert!(applied.unlinked.is_empty());
    assert_eq!(applied.nodes.len(), items.len());
    world.check(&tx);
    let names: Vec<&[u8]> = all.iter().map(Vec::as_slice).collect();
    let unlinked: Vec<Option<bool>> = (0..14).map(|n| Some(n >= 7)).collect();
    assert_eq!(shown(&tx, &names), unlinked);
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));

    let left: BTreeSet<Vec<u8>> = (7..14).map(key).collect();
    check_committed(&world, &left);
    let reader = world.begin();
    assert_eq!(shown(&reader, &names), unlinked);
    assert_eq!(db.private_ids(), 0);
}
