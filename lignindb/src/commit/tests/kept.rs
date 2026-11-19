//! A node that is written with its value kept (`ValuePut::Keep`), and a
//! commit outside the transaction that writes the node's key (D-095 item 2,
//! D-096 item 6).
//!
//! A writer that changes a hash or a link of a node and leaves its value
//! states no value: the node keeps the bytes it has. The hashes the writer
//! states were made for those bytes. So the node must never be published
//! with other bytes under them, which is what the conflict rule sees to
//! from the write on: the kept node is a write of its key (CF2).
//!
//! The rule sees a commit that wrote the key after the transaction first
//! wrote it. One that landed before the write is no conflict (case A2 of
//! `transactions.md` 16.3), and the write then keeps the value it finds,
//! which is that commit's: a writer that states hashes beside a kept value
//! has to have made them for the value the node has in the transaction's
//! view at the write (`ValuePut::Keep`). The engine does not check that
//! it has.

use std::collections::{BTreeMap, BTreeSet};

use lignin_format::tree_page::Hash;

use super::{groups, pending_held};
use crate::{
    commit::CommitError,
    txn::tests::{
        model::{self, hash_of, OwnedBatch, OwnedMerk},
        store::{Db, Outcome},
        value_of, Tx, World,
    },
};

/// The Merk of these tests, which every store has entered.
const MERK: u8 = 5;

fn key(index: u8) -> Vec<u8> {
    format!("node {index:02}").into_bytes()
}

/// The value and the value hash that `tx` shows for the node `key`.
fn shown(tx: &Tx<'_>, key: &[u8]) -> (Vec<u8>, Hash) {
    let read = |view: Option<crate::txn::NodeView<'_>>| {
        let view = view.expect("the node is there");
        (value_of(&view), *view.value_hash())
    };
    (tx.txn.node(model::subtree(MERK), key, read)).expect("a read")
}

/// The same of the latest committed state.
fn committed(world: &World<'_>, key: &[u8]) -> (Vec<u8>, Hash) {
    shown(&world.begin(), key)
}

/// A change of the Merk as `tx` reads it now, which adds a node below the
/// root: the nodes on the way to it, the root among them, get other links
/// and hashes and keep their values, and the root stays the root. Returns
/// the batch and the keys of the nodes that keep their values.
fn adding<'s>(world: &World<'s>, tx: &Tx<'s>, len: usize) -> (OwnedBatch, Vec<Vec<u8>>) {
    let mut there = BTreeSet::new();
    world.merk(tx, MERK, |items| there.extend(items.keys().cloned()));
    let merk: OwnedMerk = (0..100)
        .map(key)
        .filter(|added| !there.contains(added))
        .map(|added| {
            world.merk(tx, MERK, |items| {
                items.insert(added, vec![0xad; len]);
            })
        })
        .find(|merk| merk.root.is_none())
        .expect("a key that does not become the root");
    let kept: Vec<Vec<u8>> = (merk.nodes.iter())
        .filter(|(_, _, stated)| !stated)
        .map(|(key, ..)| key.clone())
        .collect();
    let batch = OwnedBatch {
        merks: vec![merk],
        ..OwnedBatch::default()
    };
    (batch, kept)
}

/// How the kept value is stored when the transaction writes its node.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Stored {
    /// In the node's cell.
    InCell,
    /// Out of line, and still in the log group of its commit.
    Pending,
    /// Out of line, in the value log.
    Placed,
}

/// D-095 item 2. A transaction writes a node with its value kept. A commit
/// outside it then gives the node another value. The kept node is a write
/// of its key, so the transaction's commit fails with a conflict (CF2), and
/// what is published is the other commit's node, value and hashes.
///
/// The node is the root of its Merk, and the commit outside writes that
/// node and nothing else: the kept node is the one key the two wrote.
///
/// Until its commit the transaction shows its own node: the value it kept,
/// under the hashes it stated for it, and never the foreign value under
/// them.
#[test]
fn a_kept_value_over_a_key_that_a_commit_outside_rewrote_is_a_conflict() {
    for stored in [Stored::InCell, Stored::Pending, Stored::Placed] {
        let len = if stored == Stored::InCell { 24 } else { 700 };
        let (db, versions) = Db::new(96);
        let mut world = World::new(&db, &versions);
        let committer = db.committer(&versions);
        let write_back = db.write_back(&versions);
        let items: BTreeMap<Vec<u8>, Vec<u8>> = (0..14)
            .map(|index| (key(index), vec![index; len]))
            .collect();
        let node = model::records(&items).0.expect("the Merk has a root");
        let planted = world.edit_outside(MERK, |merk| merk.clone_from(&items), &[]);
        assert!(matches!(planted, Outcome::Committed(_)), "{stored:?}");
        if stored == Stored::Placed {
            write_back.pass().expect("a pass");
        }
        let pending = pending_held(&db, &versions).len();
        assert_eq!(pending > 0, stored == Stored::Pending, "{stored:?}");

        // The transaction adds a node. The root is written with another
        // link and hash, and its value kept.
        let tx = world.begin();
        let (batch, kept) = adding(&world, &tx, len);
        assert!(kept.contains(&node), "{stored:?}: the root keeps its value");
        world.apply(&tx, &batch).expect("the change is applied");
        let (old, stated) = shown(&tx, &node);
        assert_eq!((&old, stated), (&items[&node], hash_of(&[&old])));
        world.check(&tx);

        // A commit outside gives the root another value, and writes no
        // other record. Write-back takes its turn: the pointer of the old
        // value is released.
        let foreign = vec![0xf0; len + 1];
        let other = world.begin();
        let rewrite = world.merk(&other, MERK, |items| {
            items.insert(node.clone(), foreign.clone());
        });
        assert_eq!(rewrite.nodes.len(), 1, "{stored:?}: {rewrite:?}");
        assert!(rewrite.nodes[0].2 && rewrite.root.is_none() && rewrite.removed.is_empty());
        let rewrite = OwnedBatch {
            merks: vec![rewrite],
            ..OwnedBatch::default()
        };
        world
            .apply(&other, &rewrite)
            .expect("the node is rewritten");
        let Outcome::Committed(theirs) = world.commit(other, &[&tx]) else {
            panic!("{stored:?}: the commit outside is made");
        };
        write_back.pass().expect("a pass");
        assert_eq!(
            committed(&world, &node),
            (foreign.clone(), hash_of(&[&foreign]))
        );

        // The transaction still shows its node whole: the value it kept
        // under the hashes it made for it. That node is its one met write.
        assert_eq!(shown(&tx, &node), (old.clone(), stated), "{stored:?}");
        assert_eq!(tx.txn.stats().overlapped, 1, "{stored:?}");
        assert_eq!(tx.model.borrow().met(), 1);
        world.check(&tx);
        // It goes on writing, and keeps the value once more.
        let (again, kept) = adding(&world, &tx, len);
        assert!(kept.contains(&node), "{stored:?}");
        world
            .apply(&tx, &again)
            .expect("a write of an overlapped transaction");
        assert_eq!(shown(&tx, &node), (old.clone(), stated), "{stored:?}");
        world.check(&tx);

        // Its commit is the conflict, and nothing of it is published.
        let logged = groups(&db).len();
        let ended = db.commit_as(&committer, &tx.txn, None);
        assert!(
            matches!(ended, Err(CommitError::Conflict)),
            "{stored:?}: {ended:?}"
        );
        assert_eq!(versions.latest(), theirs);
        assert_eq!(groups(&db).len(), logged);
        assert_eq!((db.pool.census().private, db.private_ids()), (0, 0));

        // Every node of the Merk has the hash of the value it has.
        let reader = world.begin();
        for key in items.keys() {
            let (value, hash) = shown(&reader, key);
            assert_eq!(hash, hash_of(&[&value]), "{stored:?}: {key:?}");
        }
        assert_eq!(
            committed(&world, &node),
            (foreign.clone(), hash_of(&[&foreign]))
        );
        tx.forget();
        world.check_committed();

        // The transaction starts again from what is committed: the root
        // keeps the value the other commit gave it, under hashes that the
        // writer made for that value.
        let (retry, kept) = adding(&world, &tx, len);
        assert!(kept.contains(&node), "{stored:?}");
        world.apply(&tx, &retry).expect("the change is applied");
        assert_eq!(shown(&tx, &node), (foreign.clone(), hash_of(&[&foreign])));
        assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
        assert_eq!(
            committed(&world, &node),
            (foreign.clone(), hash_of(&[&foreign]))
        );
        world.check_committed();
        assert!(db.flush_errors.lock().expect("the errors").is_empty());
    }
}
