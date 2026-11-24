//! A node that is written with its value kept (`ValuePut::Keep`), and the
//! commits outside the transaction that write the node's key (D-095 item 2,
//! D-096 item 6, D-097, D-098 item 7, D-100).
//!
//! A writer that changes a hash or a link of a node and leaves its value
//! states no value: the node keeps the bytes it has. The hashes the writer
//! states were made for the bytes it read. So the node must never be
//! published with other bytes under them. Two rules see to that, one for
//! each side of the write:
//!
//! - **From the write on** the kept node is a write of its key: a commit
//!   that writes the key afterwards meets it (CF2).
//! - **From the read to the write** the kept value is dated at its read
//!   (CF2a). The write names the read by the node's handle
//!   (`NodePut::at`), the transaction remembers the version each of its
//!   leases is of, and a commit that wrote the key after that version, or
//!   a range that holds it, meets the write where it is made. A handle the
//!   transaction cannot date is taken to be met, and a keep of a committed
//!   value that names no handle is refused (`BatchError::NoRead`).
//!
//! A value the transaction stated itself is its own to keep: no read is
//! named for it, and CF2 alone decides. A node it has only written with its
//! value kept has the committed value still, and a later keep of it by a
//! handle is dated at that handle's read.
//!
//! Either way the transaction's commit is a conflict, and what is
//! published is the other commit's node. Between a write that is met by
//! CF2a and that commit, the transaction itself shows the node with the
//! bytes the other commit gave it under the hashes its writer stated: the
//! bytes the writer read are gone. Nothing of it is published.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

use lignin_format::tree_page::{Hash, HashField, Side};
use lignin_store::flush::WriteBack;
use lignin_tree::node::NodeHandle;

use super::{groups, pending_held, records};
use crate::{
    commit::{CommitError, Committer},
    db::WriteError,
    snapshot::Versions,
    txn::{
        reads::KEPT_COMMITS,
        tests::{
            model::{self, handle_of, hash_of, OwnedBatch, OwnedMerk, OwnedSubtree, Val},
            put,
            store::{Db, Outcome, Rng, ENTERED},
            value_of, Tx, World,
        },
        Applied, BatchError, Column, KeySet, NodeView, Scope, TxnError, Unit, WritePath, WriteTxn,
    },
    verify::{Place, Rule},
};

/// The keys and values of one Merk.
type Items = BTreeMap<Vec<u8>, Vec<u8>>;

/// The Merk of these tests, which every store has entered.
const MERK: u8 = 5;
/// Another Merk, which no transaction of a test writes that keeps a value.
const OTHER: u8 = 6;

fn key(index: u8) -> Vec<u8> {
    format!("node {index:02}").into_bytes()
}

fn data() -> Unit {
    Unit::data(model::subtree(MERK))
}

fn aux(name: u8) -> Unit {
    model::unit(Column::Aux, name)
}

/// The value and the value hash that `tx` shows for the node `key`.
fn shown(tx: &Tx<'_>, key: &[u8]) -> (Vec<u8>, Hash) {
    let read = |view: Option<NodeView<'_>>| {
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

/// A batch that writes one node of the Merk `name`, which holds `items`,
/// with another hash and its value kept: a node with no child, so that the
/// batch names nothing else. Returns the node's key with it.
fn rehash(items: &Items, name: u8) -> (Vec<u8>, OwnedBatch) {
    let (leaf, mut node) = (model::records(items).1.into_iter())
        .find(|(_, node)| node.left.is_none() && node.right.is_none())
        .expect("a leaf");
    node.kv_hash = [0x5a; 32];
    let merk = OwnedMerk {
        nodes: vec![(leaf.clone(), node, false)],
        ..OwnedMerk::change(model::subtree(name), &BTreeMap::new(), &Items::new())
    };
    let batch = OwnedBatch {
        merks: vec![merk],
        ..OwnedBatch::default()
    };
    (leaf, batch)
}

/// `batch`, with every node that keeps its value read now through `tx`,
/// outside a scope: the read that the write then names.
fn read_now(tx: &Tx<'_>, batch: &OwnedBatch) -> OwnedBatch {
    batch.reading(|subtree, key| tx.txn.node(subtree, key, handle_of).expect("a read"))
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

/// A store with the Merk of the tests committed.
struct Bed<'s> {
    db: &'s Db,
    versions: &'s Versions,
    world: World<'s>,
    committer: Committer<'s>,
    write_back: WriteBack<'s>,
    /// The length of the values of the Merk's nodes.
    len: usize,
    items: Items,
    /// The root of the Merk: the node whose kept value the tests look at.
    node: Vec<u8>,
}

/// Runs `test` on a store whose Merk has fourteen nodes, with their
/// values stored as `stored` says.
fn bed(stored: Stored, test: impl FnOnce(&mut Bed<'_>)) {
    let len = if stored == Stored::InCell { 24 } else { 700 };
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let write_back = db.write_back(&versions);
    let items: Items = (0..14)
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
    let mut bed = Bed {
        db: &db,
        versions: &versions,
        world,
        committer: db.committer(&versions),
        write_back,
        len,
        items,
        node,
    };
    test(&mut bed);
    bed.settled();
    assert!(db.flush_errors.lock().expect("the errors").is_empty());
}

impl<'s> Bed<'s> {
    /// A commit outside every open transaction gives the root another
    /// value, and writes no other record. Write-back takes its turn: the
    /// pointer of the old value is released. Returns the new value.
    fn rewrite_outside(&mut self, open: &[&Tx<'s>]) -> Vec<u8> {
        let node = self.node.clone();
        let mut foreign = committed(&self.world, &node).0;
        foreign[0] ^= 0xff;
        let other = self.world.begin();
        let rewrite = self.world.merk(&other, MERK, |items| {
            items.insert(node.clone(), foreign.clone());
        });
        assert_eq!(rewrite.nodes.len(), 1, "{rewrite:?}");
        assert!(rewrite.nodes[0].2 && rewrite.root.is_none() && rewrite.removed.is_empty());
        let rewrite = OwnedBatch {
            merks: vec![rewrite],
            ..OwnedBatch::default()
        };
        (self.world.apply(&other, &rewrite)).expect("the node is rewritten");
        let made = self.world.commit(other, open);
        assert!(matches!(made, Outcome::Committed(_)), "{made:?}");
        self.write_back.pass().expect("a pass");
        assert_eq!(
            committed(&self.world, &node),
            (foreign.clone(), hash_of(&[&foreign]))
        );
        foreign
    }

    /// A commit outside every open transaction that writes another Merk.
    fn write_elsewhere(&mut self, open: &[&Tx<'s>]) {
        let next = self.world.latest().get() as u8;
        let edit = |items: &mut Items| {
            items.insert(vec![b'e', next], vec![next; 9]);
        };
        let made = self.world.edit_outside(OTHER, edit, open);
        assert!(matches!(made, Outcome::Committed(_)), "{made:?}");
    }

    /// A commit outside every open transaction that writes one flat
    /// record of another subtree.
    fn put_elsewhere(&mut self, open: &[&Tx<'s>]) {
        let other = self.world.begin();
        let next = self.world.latest().get().to_be_bytes();
        let writes = vec![put(aux(OTHER), &next, b"elsewhere")];
        self.world.write(&other, writes);
        let made = self.world.commit(other, open);
        assert!(matches!(made, Outcome::Committed(_)), "{made:?}");
    }

    /// A commit outside every open transaction that writes one flat
    /// record of another subtree, with `also` in its key set (CF5).
    fn cover_outside(&mut self, also: &KeySet) {
        let other = self.world.begin();
        let next = self.world.latest().get() as u8;
        let writes = vec![put(aux(OTHER), &[b'c', next], b"covered")];
        self.world.write(&other, writes.clone());
        let published = (self.db)
            .commit_as(&self.committer, &other.txn, Some(also))
            .expect("the commit is made")
            .expect("it takes a version");
        assert_eq!(self.world.model.commit(&writes), published.version.get());
    }

    /// Every node of the Merk has, in the latest committed state, the
    /// hash of the value it has.
    fn hashes_hold(&self) {
        let reader = self.world.begin();
        let mut keys = BTreeSet::new();
        self.world
            .merk(&reader, MERK, |items| keys.extend(items.keys().cloned()));
        for key in keys {
            let (value, hash) = shown(&reader, &key);
            assert_eq!(hash, hash_of(&[&value]), "{key:?}");
        }
    }

    /// Applies `batch`, which names its reads, to `tx`, and tells the
    /// model which of the nodes it keeps are met where they are written.
    fn keep(&self, tx: &Tx<'s>, batch: &OwnedBatch, met: &[Vec<u8>]) {
        // What the commits met before, which the transaction takes in at
        // the latest with this write.
        let before = tx.model.borrow().met();
        (self.world.apply_as_read(tx, batch)).expect("the write is taken");
        for key in met {
            tx.model.borrow_mut().meet_kept(data(), key);
        }
        assert_eq!(tx.txn.stats().overlapped, before + met.len());
        assert_eq!(tx.model.borrow().met(), before + met.len());
    }

    /// Commits `tx`, whose model says whether that is a conflict, and
    /// checks that a conflict published nothing.
    fn end(&mut self, tx: Tx<'s>) -> Outcome {
        let (latest, logged) = (self.versions.latest(), groups(self.db).len());
        let outcome = self.world.commit(tx, &[]);
        if outcome == Outcome::Conflict {
            assert_eq!(self.versions.latest(), latest);
            assert_eq!(groups(self.db).len(), logged);
        }
        self.settled();
        outcome
    }

    /// No transaction holds anything, every node of the Merk has the hash
    /// of its value, and the committed state is the model's.
    fn settled(&self) {
        assert_eq!(
            (self.db.pool.census().private, self.db.private_ids()),
            (0, 0)
        );
        self.hashes_hold();
        self.world.check_committed();
    }
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
        bed(stored, |bed| {
            let (node, len) = (bed.node.clone(), bed.len);
            // The transaction adds a node. The root is written with
            // another link and hash, and its value kept.
            let tx = bed.world.begin();
            let (batch, kept) = adding(&bed.world, &tx, len);
            assert!(kept.contains(&node), "{stored:?}: the root keeps its value");
            bed.world.apply(&tx, &batch).expect("the change is applied");
            let (old, stated) = shown(&tx, &node);
            assert_eq!((&old, stated), (&bed.items[&node], hash_of(&[&old])));
            bed.world.check(&tx);

            // A commit outside gives the root another value.
            let foreign = bed.rewrite_outside(&[&tx]);
            let theirs = bed.versions.latest();

            // The transaction still shows its node whole: the value it
            // kept under the hashes it made for it. That node is its one
            // met write.
            assert_eq!(shown(&tx, &node), (old.clone(), stated), "{stored:?}");
            assert_eq!(tx.txn.stats().overlapped, 1, "{stored:?}");
            assert_eq!(tx.model.borrow().met(), 1);
            bed.world.check(&tx);
            // It goes on writing, and keeps the value once more.
            let (again, kept) = adding(&bed.world, &tx, len);
            assert!(kept.contains(&node), "{stored:?}");
            (bed.world.apply(&tx, &again)).expect("a write of an overlapped transaction");
            assert_eq!(shown(&tx, &node), (old.clone(), stated), "{stored:?}");
            bed.world.check(&tx);

            // Its commit is the conflict, and nothing of it is published.
            let logged = groups(bed.db).len();
            let ended = bed.db.commit_as(&bed.committer, &tx.txn, None);
            assert!(
                matches!(ended, Err(CommitError::Conflict)),
                "{stored:?}: {ended:?}"
            );
            assert_eq!(bed.versions.latest(), theirs);
            assert_eq!(groups(bed.db).len(), logged);
            assert_eq!((bed.db.pool.census().private, bed.db.private_ids()), (0, 0));
            bed.hashes_hold();
            assert_eq!(
                committed(&bed.world, &node),
                (foreign.clone(), hash_of(&[&foreign]))
            );
            tx.forget();
            bed.world.check_committed();

            // The transaction starts again from what is committed: the
            // root keeps the value the other commit gave it, under hashes
            // that the writer made for that value.
            let (retry, kept) = adding(&bed.world, &tx, len);
            assert!(kept.contains(&node), "{stored:?}");
            bed.world.apply(&tx, &retry).expect("the change is applied");
            assert_eq!(shown(&tx, &node), (foreign.clone(), hash_of(&[&foreign])));
            assert!(matches!(bed.end(tx), Outcome::Committed(_)));
            assert_eq!(
                committed(&bed.world, &node),
                (foreign.clone(), hash_of(&[&foreign]))
            );
        });
    }
}

/// D-097, the other order. A writer reads a node, a commit outside its
/// transaction then gives the node another value, and only then the writer
/// writes the node with its value kept, under the hashes it made for the
/// value it read. The commit landed before the transaction's first write
/// of the key, so CF2 sees nothing. The write names its read, and the
/// commit wrote the key after that read: the write is met where it is
/// made (CF2a), the transaction's commit is a conflict, and what is
/// published is the other commit's node.
#[test]
fn a_kept_value_that_a_commit_outside_rewrote_before_the_write_is_a_conflict() {
    for stored in [Stored::InCell, Stored::Pending, Stored::Placed] {
        bed(stored, |bed| {
            let (node, len) = (bed.node.clone(), bed.len);
            // The writer reads the nodes it is about to change, and makes
            // its change for what it read.
            let tx = bed.world.begin();
            let (batch, kept) = adding(&bed.world, &tx, len);
            assert!(kept.contains(&node), "{stored:?}: the root keeps its value");
            let batch = read_now(&tx, &batch);
            let (old, stated) = shown(&tx, &node);
            assert_eq!((&old, stated), (&bed.items[&node], hash_of(&[&old])));
            assert_eq!(tx.txn.stats().writes, 0);

            // A commit outside gives the root another value, and the old
            // one goes: write-back releases its pointer.
            let foreign = bed.rewrite_outside(&[&tx]);
            let theirs = bed.versions.latest();
            assert_ne!(stated, hash_of(&[&foreign]));

            // The write is taken, and its node is met at once: the one
            // node of the batch whose key the commit wrote.
            bed.keep(&tx, &batch, std::slice::from_ref(&node));
            // The bytes the writer read are gone. What the transaction
            // shows until its commit is the node as it wrote it: the
            // bytes that are there, under the hashes it stated.
            assert_eq!(shown(&tx, &node), (foreign.clone(), stated), "{stored:?}");
            assert_eq!(tx.txn.stats().base, None, "it serves its records by key");

            // Its commit is the conflict, and nothing of it is published.
            let logged = groups(bed.db).len();
            let ended = bed.db.commit_as(&bed.committer, &tx.txn, None);
            assert!(
                matches!(ended, Err(CommitError::Conflict)),
                "{stored:?}: {ended:?}"
            );
            assert_eq!(bed.versions.latest(), theirs);
            assert_eq!(groups(bed.db).len(), logged);
            assert_eq!((bed.db.pool.census().private, bed.db.private_ids()), (0, 0));
            bed.hashes_hold();
            assert_eq!(
                committed(&bed.world, &node),
                (foreign.clone(), hash_of(&[&foreign]))
            );
            tx.forget();
            bed.world.check_committed();

            // The writer starts again: it reads the node as it is now.
            let (retry, kept) = adding(&bed.world, &tx, len);
            assert!(kept.contains(&node), "{stored:?}");
            bed.world.apply(&tx, &retry).expect("the change is applied");
            assert_eq!(tx.txn.stats().overlapped, 0);
            assert!(matches!(bed.end(tx), Outcome::Committed(_)));
            assert_eq!(
                committed(&bed.world, &node),
                (foreign.clone(), hash_of(&[&foreign]))
            );
        });
    }
}

/// What a writer read the nodes through whose values it then keeps.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Through {
    /// The transaction, by key, before it has written anything: committed
    /// pages under the transaction's lease.
    Txn,
    /// A scope of the transaction, which merges before the write.
    Scope,
    /// The transaction's frames: it has written before.
    Frames,
    /// The committed read (RD10), under the lease of its own.
    Committed,
    /// A cursor over the Merk, under the lease of its own.
    Cursor,
    /// The root by key, and its children through the root's handle.
    Child,
}

/// What a commit outside the transaction does between the read and the
/// write.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Between {
    /// Nothing is committed.
    Nothing,
    /// Another Merk is written.
    Elsewhere,
    /// The node gets another value.
    TheNode,
}

/// `batch`, with every node that keeps its value read now, `through` what
/// the case says.
fn read_through(tx: &Tx<'_>, root: &[u8], batch: &OwnedBatch, through: Through) -> OwnedBatch {
    let subtree = model::subtree(MERK);
    match through {
        Through::Txn | Through::Frames => read_now(tx, batch),
        Through::Scope => {
            let scope = tx.txn.scope().expect("a scope");
            let read = batch.through(&scope);
            scope.merge();
            read
        }
        Through::Committed => batch.reading(|subtree, key| {
            (tx.txn.committed_node(subtree, key, handle_of)).expect("a committed read")
        }),
        Through::Cursor => {
            let mut cursor = tx.txn.merk_cursor(subtree).expect("a cursor");
            batch.reading(|_, key| {
                cursor.seek(key).expect("a seek");
                assert_eq!(cursor.key(), Some(key));
                cursor.node(handle_of).expect("a read")
            })
        }
        Through::Child => {
            let parent = (tx.txn.node(subtree, root, handle_of))
                .expect("a read")
                .expect("the root has a handle");
            let mut children = BTreeMap::new();
            for side in [Side::Left, Side::Right] {
                let child = |view: Option<NodeView<'_>>| {
                    view.map(|view| (view.key().to_vec(), view.handle()))
                };
                let child = tx.txn.child(subtree, parent, side, child).expect("a read");
                children.extend(child.expect("the handle is good"));
            }
            let read = batch.reading(|subtree, key| match children.get(key) {
                Some(handle) => *handle,
                None if key == root => Some(parent),
                None => tx.txn.node(subtree, key, handle_of).expect("a read"),
            });
            let by_child = (read.merks[0].hints.keys())
                .filter(|key| children.contains_key(*key))
                .count();
            assert!(by_child > 0, "a kept node is a child of the root");
            read
        }
    }
}

/// A write that keeps a value by a handle of an earlier read, whatever the
/// node was read through, is met exactly where a commit wrote the node's
/// key since the read. With no commit in between, or one that wrote
/// another key, the write is as it was before the rule: no entry is met,
/// the commit is made, and the node is published with the value it had
/// under the hashes the writer stated.
#[test]
fn a_kept_value_is_met_by_the_commits_since_its_read_and_by_no_other() {
    let all = [
        Through::Txn,
        Through::Scope,
        Through::Frames,
        Through::Committed,
        Through::Cursor,
        Through::Child,
    ];
    for through in all {
        for between in [Between::Nothing, Between::Elsewhere, Between::TheNode] {
            bed(Stored::InCell, |bed| {
                let case = format!("{through:?}, {between:?}");
                let (node, len) = (bed.node.clone(), bed.len);
                let tx = bed.world.begin();
                if through == Through::Frames {
                    bed.world.write(&tx, vec![put(aux(7), b"own", b"1")]);
                    assert!(tx.txn.stats().base.is_some(), "{case}: it has frames");
                }
                let (batch, kept) = adding(&bed.world, &tx, len);
                assert!(kept.contains(&node), "{case}");
                let batch = read_through(&tx, &node, &batch, through);
                assert_eq!(batch.merks[0].hints.len(), kept.len(), "{case}");
                let (old, stated) = shown(&tx, &node);

                let met = match between {
                    Between::Nothing => Vec::new(),
                    Between::Elsewhere => {
                        bed.write_elsewhere(&[&tx]);
                        Vec::new()
                    }
                    Between::TheNode => {
                        bed.rewrite_outside(&[&tx]);
                        vec![node.clone()]
                    }
                };
                let rebuilds = tx.txn.stats().rebuilds;
                bed.keep(&tx, &batch, &met);
                if through == Through::Frames && between != Between::Nothing {
                    // The frames the node was read in went with the commit
                    // outside, and the write was made on new ones.
                    assert_eq!(tx.txn.stats().rebuilds, rebuilds + 1, "{case}");
                }
                if met.is_empty() {
                    assert_eq!(shown(&tx, &node), (old.clone(), stated), "{case}");
                    bed.world.check(&tx);
                    assert!(matches!(bed.end(tx), Outcome::Committed(_)), "{case}");
                    assert_eq!(committed(&bed.world, &node), (old, stated), "{case}");
                } else {
                    assert_eq!(bed.end(tx), Outcome::Conflict, "{case}");
                    assert_ne!(committed(&bed.world, &node).0, old, "{case}");
                }
            });
        }
    }
}

/// Whether `result` is the refusal of a keep that names no read, of a
/// node of the Merk `name`.
fn no_read<T>(result: &Result<T, TxnError>, name: u8) -> bool {
    result.as_ref().is_err_and(|error| is_no_read(error, name))
}

fn is_no_read(error: &TxnError, name: u8) -> bool {
    let subtree = model::subtree(name);
    matches!(error, TxnError::Batch(BatchError::NoRead { subtree: of }) if *of == subtree)
}

/// D-098 item 7, rule 5. A node that keeps a committed value and names no
/// handle is refused, whichever way the transaction takes its writes, and
/// the refusal changes nothing: no page, no entry of the write set, and
/// none of the other records of the batch.
#[test]
fn a_kept_value_that_names_no_read_is_refused_and_nothing_is_changed() {
    /// A change of the Merk whose kept nodes name no read, and a flat
    /// record beside it.
    fn unread<'s>(world: &World<'s>, tx: &Tx<'s>, len: usize) -> OwnedBatch {
        let (mut batch, kept) = adding(world, tx, len);
        assert!(!kept.is_empty());
        batch.records.push(put(aux(OTHER), b"beside", b"x"));
        batch
    }
    bed(Stored::InCell, |bed| {
        let (db, len) = (bed.db, bed.len);
        let held = |tx: &Tx<'_>| (tx.txn.stats(), db.pool.census().private, db.private_ids());
        let beside = |tx: &Tx<'_>| {
            let mut out = Vec::new();
            (tx.txn.get(aux(OTHER), b"beside", &mut out)).expect("a read")
        };

        // A transaction with frames.
        let tx = bed.world.begin();
        bed.world.write(&tx, vec![put(aux(7), b"own", b"1")]);
        let batch = unread(&bed.world, &tx, len);
        let before = held(&tx);
        assert!(before.0.base.is_some());
        let refused = bed.world.apply_as_read(&tx, &batch);
        assert!(no_read(&refused, MERK), "{refused:?}");
        assert_eq!(held(&tx), before);
        assert!(!beside(&tx));
        bed.world.check(&tx);

        // A scope that was opened beside another: its writes are kept in
        // the write set, by key.
        let first = tx.txn.scope().expect("a scope");
        let second = tx.txn.scope().expect("a scope");
        assert!(!second.is_laid_out());
        let before = held(&tx);
        let refused = batch.with(|batch| second.apply(batch));
        assert!(no_read(&refused, MERK), "{refused:?}");
        assert_eq!(held(&tx), before);
        second.merge();
        first.merge();
        assert!(!beside(&tx));
        bed.world.check(&tx);

        // A transaction that a commit has met, which serves its records
        // by key.
        let outside = records(vec![put(aux(7), b"own", b"2")]);
        assert!(matches!(
            bed.world.write_outside(&outside, &[&tx]),
            Outcome::Committed(_)
        ));
        let batch = unread(&bed.world, &tx, len);
        assert!(!beside(&tx));
        let before = held(&tx);
        assert_eq!((before.0.overlapped, before.0.base), (1, None));
        let refused = bed.world.apply_as_read(&tx, &batch);
        assert!(no_read(&refused, MERK), "{refused:?}");
        assert_eq!(held(&tx), before);
        assert!(!beside(&tx));
        bed.world.check(&tx);
        assert_eq!(bed.end(tx), Outcome::Conflict);

        // A transaction's first write: it is left with no write.
        let tx = bed.world.begin();
        let batch = unread(&bed.world, &tx, len);
        let refused = bed.world.apply_as_read(&tx, &batch);
        assert!(no_read(&refused, MERK), "{refused:?}");
        let after = held(&tx);
        assert_eq!(
            (after.0.writes, after.0.tree_pages, after.0.flat_pages),
            (0, 0, 0)
        );
        assert_eq!((after.1, after.2), (0, 0));
        assert!(!beside(&tx));
        // The same batch with its reads named is taken.
        bed.world.apply(&tx, &batch).expect("the reads are named");
        assert!(beside(&tx));
        bed.world.check(&tx);
        assert!(matches!(bed.end(tx), Outcome::Committed(_)));

        // A write with no transaction (RD7) is a transaction of its own
        // that has read nothing: it has no read to name.
        let reader = bed.world.begin();
        let batch = unread(&bed.world, &reader, len);
        drop(reader);
        let (latest, logged) = (bed.versions.latest(), groups(bed.db).len());
        let refused = batch.with(|batch| bed.db.write_as(&bed.committer, batch));
        assert!(
            matches!(&refused, Err(CommitError::Refused(error)) if is_no_read(error, MERK)),
            "{refused:?}"
        );
        assert_eq!(
            (bed.versions.latest(), groups(bed.db).len()),
            (latest, logged)
        );

        // A node that is not there has no value to keep, with no read
        // named as with one: that is what the batch is refused for.
        let tx = bed.world.begin();
        let (_, absent) = rehash(&bed.items, 9);
        let refused = bed.world.apply_as_read(&tx, &absent);
        let no_value = BatchError::NoValue {
            subtree: model::subtree(9),
        };
        assert!(
            matches!(&refused, Err(TxnError::Batch(error)) if *error == no_value),
            "{refused:?}"
        );
        assert_eq!(bed.end(tx), Outcome::Empty);
    });
}

/// A node that the transaction has written with its value kept has the
/// committed value still: the transaction never stated one. So a second
/// keep of it is dated at its own read, which may be the earlier one. A
/// writer reads the root, a commit outside gives it another value, a
/// second writer of the same transaction reads the root as it is now and
/// writes it with its value kept, which meets nothing. Then the first
/// writer writes what it made for the value it read: that keep is met.
#[test]
fn a_second_keep_of_a_node_is_dated_at_its_own_read() {
    for by_key in [false, true] {
        bed(Stored::InCell, |bed| {
            let node = bed.node.clone();
            let tx = bed.world.begin();
            if by_key {
                // A commit meets an earlier write of the transaction, which
                // serves its records by key from then on and takes its own
                // copy of every value it keeps.
                bed.world.write(&tx, vec![put(aux(7), b"own", b"1")]);
                let outside = records(vec![put(aux(7), b"own", b"2")]);
                let made = bed.world.write_outside(&outside, &[&tx]);
                assert!(matches!(made, Outcome::Committed(_)), "{made:?}");
            }
            let (stale, kept) = adding(&bed.world, &tx, bed.len);
            assert!(kept.contains(&node), "{by_key}");
            let stale = read_now(&tx, &stale);
            bed.rewrite_outside(&[&tx]);

            // The second writer: a fresh read, and nothing is met by it.
            let before = tx.model.borrow().met();
            let (fresh, again) = adding(&bed.world, &tx, bed.len);
            assert!(again.contains(&node), "{by_key}");
            bed.world.apply(&tx, &fresh).expect("the write is taken");
            assert_eq!(tx.txn.stats().overlapped, before, "{by_key}");
            assert_eq!(tx.txn.stats().base.is_none(), by_key);

            // The first writer, by the read from before the commit.
            bed.keep(&tx, &stale, std::slice::from_ref(&node));
            assert_eq!(bed.end(tx), Outcome::Conflict, "{by_key}");
        });
    }
}

/// A copy the transaction took of a committed value stays a copy when the
/// write set is made again around it: the rollback of another scope does
/// not make it a value the transaction stated. A scope beside another
/// reads the root, a commit outside gives the root another value, and the
/// scope reads the root as it is now and writes it with its value kept,
/// which takes a copy. The other scope, which wrote before that, is rolled
/// back or stays. Then the scope writes what it made for the value it
/// first read: that keep is met either way. With no such keep the copy is
/// committed as the value it is.
#[test]
fn a_copy_of_a_kept_value_is_no_stated_value_after_another_scope_is_rolled_back() {
    for (rolled_back, stale_keep) in [(false, true), (true, true), (true, false)] {
        bed(Stored::InCell, |bed| {
            let node = bed.node.clone();
            let tx = bed.world.begin();
            let one = tx.txn.scope().expect("a scope");
            let other = tx.txn.scope().expect("a scope");
            assert!(!other.is_laid_out());
            let flat = records(vec![put(aux(7), b"before", b"1")]);
            (bed.world.apply_in(&tx, &one, &flat)).expect("a write");
            let (stale, kept) = adding(&bed.world, &tx, bed.len);
            assert!(kept.contains(&node));
            let stale = stale.through(&other);
            bed.rewrite_outside(&[&tx]);

            let (fresh, _) = adding(&bed.world, &tx, bed.len);
            (bed.world.apply_in(&tx, &other, &fresh.through(&other))).expect("the write is taken");
            assert_eq!(tx.txn.stats().overlapped, 0, "{rolled_back}");
            let one = match rolled_back {
                true => {
                    one.rollback();
                    tx.model.borrow_mut().writes.remove(0);
                    None
                }
                false => Some(one),
            };
            if !stale_keep {
                other.merge();
                one.into_iter().for_each(Scope::merge);
                assert!(matches!(bed.end(tx), Outcome::Committed(_)));
                return;
            }

            (bed.world.apply_in(&tx, &other, &stale)).expect("the write is taken");
            tx.model.borrow_mut().meet_kept(data(), &node);
            assert_eq!(tx.txn.stats().overlapped, 1, "{rolled_back}");
            other.merge();
            one.into_iter().for_each(Scope::merge);
            assert_eq!(bed.end(tx), Outcome::Conflict, "{rolled_back}");
        });
    }
}

/// A batch that clears a Merk and keeps the value of one of its nodes has
/// no value to keep, also where the transaction wrote that node itself:
/// the clear comes first. A scope that keeps its writes in the write set
/// answers as one whose writes are on the pages.
#[test]
fn a_keep_under_a_clear_of_the_same_batch_has_no_value() {
    for beside in [false, true] {
        bed(Stored::InCell, |bed| {
            let subtree = model::subtree(9);
            let tx = bed.world.begin();
            let first = tx.txn.scope().expect("a scope");
            let second = beside.then(|| tx.txn.scope().expect("a scope"));
            let scope = second.as_ref().unwrap_or(&first);
            assert_eq!(scope.is_laid_out(), !beside);

            // The transaction makes the Merk's one node, with its value.
            let items: Items = [(b"k".to_vec(), vec![1; 9])].into();
            let made = OwnedMerk::change(subtree, &BTreeMap::new(), &items);
            let node = made.nodes[0].clone();
            let made = OwnedBatch {
                merks: vec![made],
                ..OwnedBatch::default()
            };
            (bed.world.apply_in(&tx, scope, &made)).expect("the node is written");
            let writes = tx.txn.stats().writes;

            // It clears the Merk and writes the node again, its value kept.
            let again = OwnedBatch {
                merks: vec![OwnedMerk {
                    nodes: vec![(node.0, node.1, false)],
                    root: Some(Some(b"k".to_vec())),
                    ..OwnedMerk::change(subtree, &BTreeMap::new(), &Items::new())
                }],
                subtrees: vec![OwnedSubtree::Clear(subtree)],
                ..OwnedBatch::default()
            };
            let refused = bed.world.apply_in(&tx, scope, &again);
            let no_value = BatchError::NoValue { subtree };
            assert!(
                matches!(&refused, Err(TxnError::Batch(error)) if *error == no_value),
                "{beside}: {refused:?}"
            );
            assert_eq!(tx.txn.stats().writes, writes, "{beside}");
            second.into_iter().for_each(Scope::merge);
            first.merge();
            bed.world.check(&tx);
            assert!(matches!(bed.end(tx), Outcome::Committed(_)), "{beside}");
        });
    }
}

/// A value the transaction wrote itself is its own to keep: the write that
/// keeps it names no read, or any, and the conflict rule decides as it did
/// (CF2), by the commits since the transaction first wrote the key.
#[test]
fn a_kept_value_of_the_transactions_own_names_no_read_and_follows_cf2() {
    /// When a commit outside gives the node another value.
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Foreign {
        Never,
        /// Before the transaction writes the node's value: case A2 of
        /// 16.3, no conflict.
        BeforeItsWrite,
        /// After it: the conflict of CF2.
        AfterItsWrite,
    }
    for foreign in [
        Foreign::Never,
        Foreign::BeforeItsWrite,
        Foreign::AfterItsWrite,
    ] {
        bed(Stored::InCell, |bed| {
            let (node, len) = (bed.node.clone(), bed.len);
            let own = vec![0x0e; len];
            let tx = bed.world.begin();
            // A handle from before everything, which the keep names in the
            // end: it is of a read that a commit has since written under.
            let stale = (tx.txn.node(model::subtree(MERK), &node, handle_of)).expect("a read");
            assert!(stale.is_some());
            if foreign == Foreign::BeforeItsWrite {
                bed.rewrite_outside(&[&tx]);
            }
            // The transaction gives the node a value of its own.
            bed.world.edit(&tx, MERK, |items| {
                items.insert(node.clone(), own.clone());
            });
            if foreign == Foreign::AfterItsWrite {
                bed.rewrite_outside(&[&tx]);
                assert_eq!(tx.model.borrow().met(), 1);
            }
            // It writes the node again and keeps the value: once with no
            // read named for it, and once by the stale handle. The other
            // nodes of the batch keep committed values, and name theirs.
            for named in [None, stale] {
                let (batch, kept) = adding(&bed.world, &tx, len);
                assert!(kept.contains(&node) && kept.len() > 1, "{foreign:?}");
                let mut batch = read_now(&tx, &batch);
                match named {
                    None => batch.merks[0].hints.remove(&node),
                    Some(handle) => batch.merks[0].hints.insert(node.clone(), handle),
                };
                bed.keep(&tx, &batch, &[]);
                assert_eq!(shown(&tx, &node), (own.clone(), hash_of(&[&own])));
                bed.world.check(&tx);
            }
            match foreign {
                Foreign::AfterItsWrite => {
                    assert_eq!(bed.end(tx), Outcome::Conflict, "{foreign:?}");
                    assert_ne!(committed(&bed.world, &node).0, own);
                }
                Foreign::Never | Foreign::BeforeItsWrite => {
                    assert!(matches!(bed.end(tx), Outcome::Committed(_)), "{foreign:?}");
                    assert_eq!(
                        committed(&bed.world, &node),
                        (own.clone(), hash_of(&[&own]))
                    );
                }
            }
        });
    }
}

/// CF5 for a kept value: a commit that covers the node's key without
/// naming it, by a range, by every key, by a clear of the Merk or by a drop
/// of its subtree, wrote the key as far as the rule goes. A keep by a
/// handle from before it is met.
#[test]
fn a_kept_value_is_met_by_a_range_a_clear_and_a_drop_since_its_read() {
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Covers {
        Range,
        RangeBeside,
        Everything,
        Subtree,
        SubtreeBeside,
        Clear,
        DropAndEnterAgain,
    }
    let all = [
        Covers::Range,
        Covers::RangeBeside,
        Covers::Everything,
        Covers::Subtree,
        Covers::SubtreeBeside,
        Covers::Clear,
        Covers::DropAndEnterAgain,
    ];
    for covers in all {
        bed(Stored::InCell, |bed| {
            let (node, len) = (bed.node.clone(), bed.len);
            let subtree = model::subtree(MERK);
            let tx = bed.world.begin();
            let (batch, kept) = adding(&bed.world, &tx, len);
            let batch = read_now(&tx, &batch);
            let (old, stated) = shown(&tx, &node);

            // The Merk with the same keys and other values: what a commit
            // that empties it puts there.
            let fresh: Items = (bed.items.keys())
                .map(|key| (key.clone(), vec![0xc1; len]))
                .collect();
            let again = |leave: Vec<OwnedSubtree>| OwnedBatch {
                merks: vec![OwnedMerk::change(subtree, &BTreeMap::new(), &fresh)],
                subtrees: leave,
                ..OwnedBatch::default()
            };
            let mut also = KeySet::new();
            let met = match covers {
                Covers::Range => {
                    // The range holds the root's key and no other kept one.
                    let mut end = node.clone();
                    end.push(0);
                    also.range(data(), &node, Some(&end));
                    bed.cover_outside(&also);
                    vec![node.clone()]
                }
                Covers::RangeBeside => {
                    // A range of another column of the subtree holds none.
                    also.range(aux(MERK), &[], None);
                    bed.cover_outside(&also);
                    Vec::new()
                }
                Covers::Everything => {
                    also.everything();
                    bed.cover_outside(&also);
                    kept.clone()
                }
                Covers::Subtree => {
                    // The subtree as a commit that drops it names it, with
                    // no node of it touched.
                    also.subtree(subtree);
                    bed.cover_outside(&also);
                    kept.clone()
                }
                Covers::SubtreeBeside => {
                    also.subtree(model::subtree(OTHER));
                    bed.cover_outside(&also);
                    Vec::new()
                }
                Covers::Clear => {
                    let batch = again(vec![OwnedSubtree::Clear(subtree)]);
                    let made = bed.world.write_outside(&batch, &[&tx]);
                    assert!(matches!(made, Outcome::Committed(_)), "{made:?}");
                    kept.clone()
                }
                Covers::DropAndEnterAgain => {
                    let enter = OwnedSubtree::Enter(subtree, None);
                    let batch = again(vec![enter, OwnedSubtree::Drop(subtree)]);
                    let made = bed.world.write_outside(&batch, &[&tx]);
                    assert!(matches!(made, Outcome::Committed(_)), "{made:?}");
                    kept.clone()
                }
            };
            bed.keep(&tx, &batch, &met);
            match met.is_empty() {
                true => {
                    assert!(matches!(bed.end(tx), Outcome::Committed(_)), "{covers:?}");
                    assert_eq!(committed(&bed.world, &node), (old, stated), "{covers:?}");
                }
                false => assert_eq!(bed.end(tx), Outcome::Conflict, "{covers:?}"),
            }
        });
    }
}

/// The batch that takes the node `leaf` of the Merk away: what a commit
/// outside the transaction of these tests does between a read and a keep.
fn taking(bed: &Bed<'_>, takes: Takes, leaf: &[u8]) -> OwnedBatch {
    let subtree = model::subtree(MERK);
    match takes {
        Takes::Delete => {
            let other = bed.world.begin();
            let merk = bed.world.merk(&other, MERK, |items| {
                items.remove(leaf);
            });
            OwnedBatch {
                merks: vec![merk],
                ..OwnedBatch::default()
            }
        }
        Takes::Clear => OwnedBatch {
            subtrees: vec![OwnedSubtree::Clear(subtree)],
            ..OwnedBatch::default()
        },
        Takes::Drop => OwnedBatch {
            subtrees: vec![OwnedSubtree::Drop(subtree)],
            ..OwnedBatch::default()
        },
    }
}

/// Applies `batch` through a scope of `tx` that is opened beside another,
/// so the write is taken by key, and rolls both scopes back.
fn apply_beside<'s>(
    world: &World<'s>,
    tx: &Tx<'s>,
    batch: &OwnedBatch,
) -> Result<Applied, TxnError> {
    let first = tx.txn.scope().expect("a scope");
    let second = tx.txn.scope().expect("a scope beside it");
    assert!(!second.is_laid_out());
    let applied = world.apply_in(tx, &second, batch);
    second.rollback();
    first.rollback();
    applied
}

/// How a commit takes a node away.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Takes {
    Delete,
    Clear,
    Drop,
}

/// A commit outside that leaves no node under the key leaves no value to
/// keep, and the write is refused: no keep with nothing to keep enters the
/// write set. That is a delete of the node, a clear of its Merk, and a drop
/// of its subtree.
///
/// Where the keep names a read the transaction can date, the refusal says
/// that the commit is its cause ([`BatchError::ValueTaken`]), which is a
/// conflict to the caller: on the frames, and where the write is taken by
/// key. Where the transaction cannot tell, the refusal is of what is not
/// there, as it is with no commit in between: the keep names a read the
/// transaction cannot date, or none.
#[test]
fn a_kept_value_whose_node_a_commit_outside_took_away_is_refused() {
    /// What the write says of its read, and where it goes.
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Read {
        /// It names a read the transaction can date.
        Dated,
        /// The same, in a scope that was opened beside another: the write
        /// is taken by key.
        DatedByKey,
        /// It names a handle from before a rollback of the transaction.
        Undatable,
        /// It names none.
        Unnamed,
    }
    for takes in [Takes::Delete, Takes::Clear, Takes::Drop] {
        for read in [
            Read::Dated,
            Read::DatedByKey,
            Read::Undatable,
            Read::Unnamed,
        ] {
            bed(Stored::InCell, |bed| {
                let at = format!("{takes:?}, {read:?}");
                let subtree = model::subtree(MERK);
                let tx = bed.world.begin();
                let (leaf, batch) = rehash(&bed.items, MERK);
                let batch = match read {
                    Read::Unnamed => batch,
                    _ => read_now(&tx, &batch),
                };
                assert_eq!(
                    batch.merks[0].hints.len(),
                    usize::from(read != Read::Unnamed)
                );
                if read == Read::Undatable {
                    bed.committer.rollback(&tx.txn).expect("no scope is open");
                }

                let outside = taking(bed, takes, &leaf);
                let made = bed.world.write_outside(&outside, &[&tx]);
                assert!(matches!(made, Outcome::Committed(_)), "{at}: {made:?}");

                let refused = match read {
                    Read::DatedByKey => apply_beside(&bed.world, &tx, &batch),
                    _ => bed.world.apply_as_read(&tx, &batch),
                };
                let refused = refused.expect_err("there is no value to keep");
                let told = matches!(read, Read::Dated | Read::DatedByKey);
                let expected = match (told, takes) {
                    (true, _) => BatchError::ValueTaken { subtree },
                    (false, Takes::Drop) => BatchError::NotEntered { subtree },
                    (false, _) => BatchError::NoValue { subtree },
                };
                assert!(
                    matches!(&refused, TxnError::Batch(error) if *error == expected),
                    "{at}: {refused:?}"
                );
                // The caller is told a conflict where the transaction can
                // tell one, by the write and by a commit that hands the
                // refusal on. The transaction is open either way.
                assert_eq!(refused.is_conflict(), told, "{at}");
                let handed_on = WriteError::Commit(CommitError::Refused(refused));
                assert_eq!(handed_on.is_conflict(), told, "{at}");
                assert!(handed_on.leaves_transaction_open(), "{at}");
                assert_eq!(tx.txn.stats().writes, 0, "{at}");
                assert_eq!(bed.end(tx), Outcome::Empty, "{at}");
            });
        }
    }
}

/// A refusal that is the transaction's own doing says nothing of a commit
/// outside, also where one took the node away after the read the keep
/// names: the transaction cleared the Merk itself, in an earlier write or
/// in the batch that keeps the value. What it cleared has no value.
#[test]
fn a_keep_of_what_the_transaction_cleared_itself_is_refused_as_that() {
    for in_the_batch in [false, true] {
        bed(Stored::InCell, |bed| {
            let subtree = model::subtree(MERK);
            let tx = bed.world.begin();
            let (leaf, batch) = rehash(&bed.items, MERK);
            let batch = read_now(&tx, &batch);
            let outside = taking(bed, Takes::Delete, &leaf);
            let made = bed.world.write_outside(&outside, &[&tx]);
            assert!(matches!(made, Outcome::Committed(_)), "{made:?}");

            let clear = OwnedBatch {
                subtrees: vec![OwnedSubtree::Clear(subtree)],
                ..OwnedBatch::default()
            };
            let batch = match in_the_batch {
                // The node is the root of the Merk the batch has cleared.
                true => OwnedBatch {
                    merks: vec![OwnedMerk {
                        root: Some(Some(leaf.clone())),
                        ..batch.merks[0].clone()
                    }],
                    subtrees: clear.subtrees.clone(),
                    ..OwnedBatch::default()
                },
                false => {
                    (bed.world.apply(&tx, &clear)).expect("the Merk is cleared");
                    batch
                }
            };
            let writes = tx.txn.stats().writes;
            let refused = bed.world.apply_as_read(&tx, &batch);
            let no_value = BatchError::NoValue { subtree };
            assert!(
                matches!(&refused, Err(TxnError::Batch(error)) if *error == no_value),
                "{in_the_batch}: {refused:?}"
            );
            assert!(!refused.expect_err("refused").is_conflict());
            assert_eq!(tx.txn.stats().writes, writes, "{in_the_batch}");
            let ended = bed.end(tx);
            match in_the_batch {
                true => assert_eq!(ended, Outcome::Empty),
                false => assert!(matches!(ended, Outcome::Committed(_)), "{ended:?}"),
            }
        });
    }
}

/// What the transaction dropped or cleared itself hides the Merks below
/// it, and a keep in one of them is refused for that, on the frames and
/// where the write is taken by key: also where a commit outside took the
/// node away after the read the keep names.
#[test]
fn a_keep_below_what_the_transaction_took_away_itself_is_refused_as_that() {
    const OUTER: u8 = ENTERED + 1;
    const INNER: u8 = ENTERED + 2;

    /// A change of the Merk `INNER`, with the node of `OUTER` that hosts
    /// it written again, as one batch. With `enter` the batch enters both
    /// subtrees first.
    fn nested<'s>(
        world: &World<'s>,
        tx: &Tx<'s>,
        enter: bool,
        edit: impl FnOnce(&mut Items),
    ) -> OwnedBatch {
        let below = world.merk(tx, INNER, edit);
        let stamp = below.nodes.len() as u8;
        let mut above = world.merk(tx, OUTER, |items| {
            let mut value = items.get(&b"host"[..]).cloned().unwrap_or_default();
            value.push(stamp);
            items.insert(b"host".to_vec(), value);
        });
        (above.hosted).insert(b"host".to_vec(), vec![model::subtree(INNER)]);
        let host = (model::subtree(OUTER), b"host".to_vec(), 0);
        let subtrees = match enter {
            true => vec![
                OwnedSubtree::Enter(model::subtree(OUTER), None),
                OwnedSubtree::Enter(model::subtree(INNER), Some(host)),
            ],
            false => Vec::new(),
        };
        OwnedBatch {
            merks: vec![below, above],
            subtrees,
            ..OwnedBatch::default()
        }
    }

    for (drops, by_key) in [(false, false), (false, true), (true, false), (true, true)] {
        let at = format!("drops: {drops}, by key: {by_key}");
        let (db, versions) = Db::new(96);
        let mut world = World::new(&db, &versions);
        let (outer, inner) = (model::subtree(OUTER), model::subtree(INNER));
        let items: Items = (0..6).map(|index| (key(index), vec![index; 20])).collect();
        let first = world.begin();
        let made = nested(&world, &first, true, |merk| merk.clone_from(&items));
        (world.apply(&first, &made)).expect("the two Merks are made");
        let made = world.commit(first, &[]);
        assert!(matches!(made, Outcome::Committed(_)), "{at}: {made:?}");

        // The transaction reads a node of the Merk below, and a commit
        // outside deletes that node.
        let tx = world.begin();
        let (leaf, batch) = rehash(&items, INNER);
        let batch = read_now(&tx, &batch);
        assert_eq!(batch.merks[0].hints.len(), 1, "{at}");
        let other = world.begin();
        let deleted = nested(&world, &other, false, |merk| {
            merk.remove(&leaf);
        });
        (world.apply(&other, &deleted)).expect("the node is deleted");
        let made = world.commit(other, &[&tx]);
        assert!(matches!(made, Outcome::Committed(_)), "{at}: {made:?}");

        // It takes the Merk above away itself, and then keeps the value
        // of the node it read.
        let leave = match drops {
            true => OwnedSubtree::Drop(outer),
            false => OwnedSubtree::Clear(outer),
        };
        let leave = OwnedBatch {
            subtrees: vec![leave],
            ..OwnedBatch::default()
        };
        (world.apply(&tx, &leave)).expect("the Merk above goes");
        let writes = tx.txn.stats().writes;
        let refused = match by_key {
            true => apply_beside(&world, &tx, &batch),
            false => world.apply_as_read(&tx, &batch),
        };
        let refused = refused.expect_err("the Merk is not there for it");
        let expected = match by_key {
            true => BatchError::NoValue { subtree: inner },
            false => BatchError::NotEntered { subtree: inner },
        };
        assert!(
            matches!(&refused, TxnError::Batch(error) if *error == expected),
            "{at}: {refused:?}"
        );
        assert!(!refused.is_conflict(), "{at}");
        assert_eq!(tx.txn.stats().writes, writes, "{at}");
        let made = world.commit(tx, &[]);
        assert!(matches!(made, Outcome::Committed(_)), "{at}: {made:?}");
        world.check_committed();
    }
}

/// A refusal says that a commit outside is its cause only for a node which
/// such a commit took away. A batch keeps the value of the root, which a
/// commit outside rewrote after it was read and which is there still, and
/// the value of a node that was never there. It is refused for the second,
/// as a batch that got something wrong.
#[test]
fn a_refusal_for_a_node_that_was_never_there_says_nothing_of_a_commit_outside() {
    bed(Stored::InCell, |bed| {
        let subtree = model::subtree(MERK);
        let tx = bed.world.begin();
        let (batch, kept) = adding(&bed.world, &tx, bed.len);
        assert!(kept.contains(&bed.node));
        let mut batch = read_now(&tx, &batch);
        bed.rewrite_outside(&[&tx]);

        // A node with no child under a key the Merk does not hold, its
        // value kept, by the handle of a read of now.
        let (leaf, rehashed) = rehash(&bed.items, MERK);
        let never = b"never there".to_vec();
        let now = (tx.txn.node(subtree, &leaf, handle_of)).expect("a read");
        let merk = &mut batch.merks[0];
        merk.nodes.insert(
            0,
            (never.clone(), rehashed.merks[0].nodes[0].1.clone(), false),
        );
        merk.hints.insert(never, now.expect("the leaf is there"));

        let refused = bed.world.apply_as_read(&tx, &batch);
        let no_value = BatchError::NoValue { subtree };
        assert!(
            matches!(&refused, Err(TxnError::Batch(error)) if *error == no_value),
            "{refused:?}"
        );
        assert_eq!(tx.txn.stats().writes, 0);
        assert_eq!(bed.end(tx), Outcome::Empty);
    });
}

/// A refusal says that commits outside are its cause only where that
/// holds for every node the batch has no value to keep of. A batch keeps
/// the value of a node that a commit outside deleted after it was read,
/// and after that node the value of one that was never there. It is
/// refused as a batch that got something wrong, on the frames and where
/// the write is taken by key.
#[test]
fn a_refusal_for_a_taken_node_and_one_that_was_never_there_is_plain() {
    for by_key in [false, true] {
        bed(Stored::InCell, |bed| {
            let subtree = model::subtree(MERK);
            let tx = bed.world.begin();
            let (leaf, batch) = rehash(&bed.items, MERK);
            let mut batch = read_now(&tx, &batch);
            assert_eq!(batch.merks[0].hints.len(), 1, "{by_key}");
            let outside = taking(bed, Takes::Delete, &leaf);
            let made = bed.world.write_outside(&outside, &[&tx]);
            assert!(matches!(made, Outcome::Committed(_)), "{by_key}: {made:?}");

            // After it in the batch, a node under a key the Merk does not
            // hold, its value kept, by the handle of a read of now.
            let never = [&leaf[..], b"+"].concat();
            assert!(!bed.items.contains_key(&never));
            let now = (tx.txn.node(subtree, &bed.node, handle_of)).expect("a read");
            let merk = &mut batch.merks[0];
            let node = merk.nodes[0].1.clone();
            merk.nodes.push((never.clone(), node, false));
            merk.hints.insert(never, now.expect("the root is there"));
            assert_eq!(merk.nodes[0].0, leaf, "the node that was taken is first");

            let refused = match by_key {
                true => apply_beside(&bed.world, &tx, &batch),
                false => bed.world.apply_as_read(&tx, &batch),
            };
            let no_value = BatchError::NoValue { subtree };
            assert!(
                matches!(&refused, Err(TxnError::Batch(error)) if *error == no_value),
                "{by_key}: {refused:?}"
            );
            assert_eq!(tx.txn.stats().writes, 0, "{by_key}");
            assert_eq!(bed.end(tx), Outcome::Empty, "{by_key}");
        });
    }
}

/// A node whose value the batch keeps and that is there says nothing of
/// the refusal, which is of the nodes that are not there. A batch keeps
/// the value of a node that a commit outside deleted after it was read,
/// and of another node that no commit wrote since. It is refused as a
/// conflict, on the frames and where the write is taken by key.
#[test]
fn a_refusal_for_a_taken_node_is_a_conflict_beside_a_kept_node_that_is_there() {
    for by_key in [false, true] {
        bed(Stored::InCell, |bed| {
            let subtree = model::subtree(MERK);
            let tx = bed.world.begin();
            // Two nodes with no child, each written with another hash and
            // its value kept, and read now.
            let mut leaves = (model::records(&bed.items).1.into_iter())
                .filter(|(_, node)| node.left.is_none() && node.right.is_none());
            let taken = leaves.next().expect("a leaf");
            let there = leaves.next().expect("another leaf");
            let nodes = [taken.clone(), there.clone()].map(|(key, mut node)| {
                node.kv_hash = [0x5a; 32];
                (key, node, false)
            });
            let merk = OwnedMerk {
                nodes: nodes.into(),
                ..OwnedMerk::change(subtree, &BTreeMap::new(), &Items::new())
            };
            let batch = OwnedBatch {
                merks: vec![merk],
                ..OwnedBatch::default()
            };
            let batch = read_now(&tx, &batch);
            assert_eq!(batch.merks[0].hints.len(), 2, "{by_key}");

            // A commit outside deletes the first, and writes the nodes
            // above it: the second is no node of those.
            let outside = taking(bed, Takes::Delete, &taken.0);
            assert!(outside.merks[0].removed.contains(&taken.0), "{by_key}");
            assert!(
                (outside.merks[0].nodes.iter()).all(|(key, ..)| *key != there.0),
                "{by_key}"
            );
            let made = bed.world.write_outside(&outside, &[&tx]);
            assert!(matches!(made, Outcome::Committed(_)), "{by_key}: {made:?}");

            let refused = match by_key {
                true => apply_beside(&bed.world, &tx, &batch),
                false => bed.world.apply_as_read(&tx, &batch),
            };
            let value_taken = BatchError::ValueTaken { subtree };
            assert!(
                matches!(&refused, Err(TxnError::Batch(error)) if *error == value_taken),
                "{by_key}: {refused:?}"
            );
            assert_eq!(tx.txn.stats().writes, 0, "{by_key}");
            assert_eq!(bed.end(tx), Outcome::Empty, "{by_key}");
        });
    }
}

/// A transaction with one write that CF2a met: it read the root, a commit
/// outside rewrote the root, and it wrote the root with its value kept.
/// Returns the transaction, the batch of that write, and how many writes
/// the batch is in the model.
fn met_by_its_read<'s>(bed: &mut Bed<'s>, tx: &Tx<'s>) -> (OwnedBatch, usize) {
    let (batch, kept) = adding(&bed.world, tx, bed.len);
    assert!(kept.contains(&bed.node));
    let batch = read_now(tx, &batch);
    bed.rewrite_outside(&[tx]);
    (batch.clone(), batch.writes().len())
}

/// A write that CF2a met stays met while it is in the write set: through
/// the transaction's later operations and the commits beside them, and
/// through a rollback to a savepoint that was set after it. A rollback to a
/// savepoint from before it takes it out, and the mark with it. The same
/// write made again by the same read is met again, and made by a new read
/// it is not.
#[test]
fn a_write_that_its_read_met_stays_met_until_it_is_taken_out() {
    bed(Stored::InCell, |bed| {
        let node = bed.node.clone();
        let tx = bed.world.begin();
        tx.txn.set_savepoint().expect("no scope is open");
        let (batch, writes) = met_by_its_read(bed, &tx);
        bed.keep(&tx, &batch, std::slice::from_ref(&node));
        assert_eq!(tx.model.borrow().writes.len(), writes);

        // Later operations, and commits beside them.
        tx.txn.set_savepoint().expect("no scope is open");
        bed.world.write(&tx, vec![put(aux(7), b"later", b"1")]);
        bed.write_elsewhere(&[&tx]);
        bed.world.write(&tx, vec![put(aux(7), b"last", b"2")]);
        assert_eq!(tx.txn.stats().overlapped, 1);
        assert_eq!(tx.txn.stats().base, None, "it serves its records by key");

        // Back to the savepoint after the write: the mark stands.
        tx.txn.rollback_to_savepoint().expect("a savepoint is set");
        tx.model.borrow_mut().writes.truncate(writes);
        let stats = tx.txn.stats();
        assert_eq!((stats.writes, stats.overlapped), (writes, 1));

        // Back to the one before it: the write is out, and nothing is met.
        tx.txn.rollback_to_savepoint().expect("a savepoint is set");
        tx.forget();
        let stats = tx.txn.stats();
        assert_eq!((stats.writes, stats.overlapped), (0, 0));
        bed.world.check(&tx);

        // The same write by the same read: the transaction still knows
        // when that read was, and the write is met again.
        tx.txn.set_savepoint().expect("no scope is open");
        bed.keep(&tx, &batch, std::slice::from_ref(&node));
        tx.txn.rollback_to_savepoint().expect("a savepoint is set");
        tx.forget();
        assert_eq!(tx.txn.stats().overlapped, 0);

        // The writer reads again and writes what it reads now.
        let (retry, kept) = adding(&bed.world, &tx, bed.len);
        assert!(kept.contains(&node));
        bed.world.apply(&tx, &retry).expect("the change is applied");
        let stats = tx.txn.stats();
        assert_eq!(stats.overlapped, 0);
        assert!(stats.base.is_some(), "it has frames again");
        bed.world.check(&tx);
        assert!(matches!(bed.end(tx), Outcome::Committed(_)));
    });
}

/// A handle that a write reports names where the write put its node, and
/// is no read. Where the write kept a value that a commit had replaced,
/// its node has the commit's bytes under the writer's hashes, at the
/// write's own version: a later keep by that handle would be dated after
/// the commit. While the write stands, its node is the transaction's own
/// and the write is met. Once a rollback has taken the write out, the
/// handle must not pass for a read: a keep by it is taken to be met.
#[test]
fn a_handle_that_a_met_write_reported_dates_no_read() {
    for framed in [false, true] {
        bed(Stored::InCell, |bed| {
            let node = bed.node.clone();
            let tx = bed.world.begin();
            let own = usize::from(framed);
            if framed {
                bed.world.write(&tx, vec![put(aux(7), b"own", b"1")]);
            }
            let (batch, kept) = adding(&bed.world, &tx, bed.len);
            assert!(kept.contains(&node));
            let batch = read_now(&tx, &batch);
            bed.rewrite_outside(&[&tx]);
            tx.txn.set_savepoint().expect("no scope is open");
            if framed {
                // A read through the frames of the version the write is
                // made at: their lease is dated by it.
                let value = |view: Option<NodeView<'_>>| view.map(|view| value_of(&view));
                (tx.txn.node(model::subtree(MERK), &key(0), value)).expect("a read");
                assert!(tx.txn.stats().base.is_some(), "{framed}: it has frames");
            }

            // The write is met, and reports where its nodes are.
            let applied = (bed.world.apply_as_read(&tx, &batch)).expect("the write is taken");
            tx.model.borrow_mut().meet_kept(data(), &node);
            assert_eq!(tx.txn.stats().overlapped, 1, "{framed}");
            let mut again = batch.clone();
            let merk = &mut again.merks[0];
            assert_eq!(applied.nodes.len(), merk.nodes.len(), "{framed}");
            for ((key, _, stated), handle) in merk.nodes.iter().zip(&applied.nodes) {
                if !stated {
                    merk.hints.insert(key.clone(), *handle);
                }
            }

            // The write is taken back, and nothing is met.
            tx.txn.rollback_to_savepoint().expect("a savepoint is set");
            tx.model.borrow_mut().writes.truncate(own);
            let stats = tx.txn.stats();
            assert_eq!((stats.writes, stats.overlapped), (own, 0), "{framed}");

            // The same write by the handles the first one reported: every
            // keep of it is taken to be met.
            bed.keep(&tx, &again, &kept);
            assert_eq!(bed.end(tx), Outcome::Conflict, "{framed}");
        });
    }
}

/// A write that is met ends no read that was made before it. It is made
/// under a lease of its own, and what the transaction read before it, of
/// the committed pages its frames are then made over or through frames it
/// had, is dated as it was. Once a rollback has taken the met write out, a
/// keep by the handles of such a read is answered from the commits since
/// that read, and the transaction commits.
#[test]
fn a_read_from_before_a_met_write_is_dated_as_it_was_once_the_write_is_out() {
    for framed in [false, true] {
        bed(Stored::InCell, |bed| {
            let node = bed.node.clone();
            let tx = bed.world.begin();
            let own = usize::from(framed);
            // The read that a commit outside meets.
            let (batch, kept) = adding(&bed.world, &tx, bed.len);
            assert!(kept.contains(&node));
            let early = read_now(&tx, &batch);
            bed.rewrite_outside(&[&tx]);
            if framed {
                bed.world.write(&tx, vec![put(aux(7), b"own", b"1")]);
            }

            // The same change read again, at the version the met write is
            // made at: of the committed pages, by a transaction that has
            // no frames yet, or through its frames.
            let (batch, kept) = adding(&bed.world, &tx, bed.len);
            assert!(kept.contains(&node));
            let late = read_now(&tx, &batch);
            assert_eq!(tx.txn.stats().base.is_some(), framed);
            tx.txn.set_savepoint().expect("no scope is open");

            // The write by the early read is met, and is taken back.
            bed.keep(&tx, &early, std::slice::from_ref(&node));
            tx.txn.rollback_to_savepoint().expect("a savepoint is set");
            tx.model.borrow_mut().writes.truncate(own);
            let stats = tx.txn.stats();
            assert_eq!((stats.writes, stats.overlapped), (own, 0), "{framed}");

            // The write by the later read: no commit wrote a key of it
            // since, and nothing is met.
            bed.keep(&tx, &late, &[]);
            assert!(matches!(bed.end(tx), Outcome::Committed(_)), "{framed}");
        });
    }
}

/// The transaction goes on watching when a rollback takes its last write
/// out, while it remembers a lease: the key sets of the commits after that
/// are what a keep by an earlier handle is answered from.
#[test]
fn a_read_is_answered_for_after_a_rollback_took_every_write_out() {
    bed(Stored::InCell, |bed| {
        let node = bed.node.clone();
        let tx = bed.world.begin();
        let (batch, kept) = adding(&bed.world, &tx, bed.len);
        assert!(kept.contains(&node));
        let batch = read_now(&tx, &batch);
        let (old, stated) = shown(&tx, &node);
        tx.txn.set_savepoint().expect("no scope is open");
        bed.world.write(&tx, vec![put(aux(7), b"own", b"1")]);
        tx.txn.rollback_to_savepoint().expect("a savepoint is set");
        tx.forget();
        assert_eq!(tx.txn.stats().writes, 0);
        assert!(tx.txn.watches());
        // Two commits elsewhere: a watch that started anew would be given
        // the newer one only.
        bed.write_elsewhere(&[&tx]);
        bed.write_elsewhere(&[&tx]);
        bed.keep(&tx, &batch, &[]);
        assert!(matches!(bed.end(tx), Outcome::Committed(_)));
        assert_eq!(committed(&bed.world, &node), (old, stated));
    });
}

/// D-155 item 6. A transaction that has been shown a node and has no
/// write dates its reads with the newest key sets, and holds no more of
/// them back, however many commits land beside it while it runs no
/// operation. A keep by the handle of a read that lies further back is
/// met then, as a read the transaction cannot date. A transaction whose
/// writes were all taken back is such a transaction again.
///
/// A transaction with a write holds every key set since, and the conflict
/// rule decides exactly (CF3): a commit beside a thousand others that
/// wrote its key is a conflict, and with none that did its commit is made.
#[test]
fn a_transaction_with_no_write_holds_the_newest_key_sets_and_one_with_a_write_all() {
    const BESIDE: usize = 1000;
    let mine = || put(aux(MERK), b"mine", b"written first");

    for wrote_before in [false, true] {
        bed(Stored::InCell, |bed| {
            let tx = bed.world.begin();
            let (batch, kept) = adding(&bed.world, &tx, bed.len);
            let batch = read_now(&tx, &batch);
            if wrote_before {
                // A write, which a rollback to a savepoint takes out.
                tx.txn.set_savepoint().expect("no scope is open");
                bed.world.write(&tx, vec![mine()]);
                bed.put_elsewhere(&[&tx]);
                tx.txn.rollback_to_savepoint().expect("a savepoint is set");
                tx.forget();
            }
            assert_eq!(tx.txn.stats().writes, 0);
            assert!(tx.txn.watches());
            for _ in 0..BESIDE {
                bed.put_elsewhere(&[&tx]);
                assert!(bed.db.commits.kept() <= KEPT_COMMITS, "{wrote_before}");
            }
            assert_eq!(bed.db.commits.kept(), KEPT_COMMITS, "{wrote_before}");
            // No commit wrote a node of the Merk, and every keep is met.
            bed.keep(&tx, &batch, &kept);
            assert_eq!(bed.end(tx), Outcome::Conflict, "{wrote_before}");
        });
    }

    for theirs in [false, true] {
        bed(Stored::InCell, |bed| {
            let tx = bed.world.begin();
            bed.world.write(&tx, vec![mine()]);
            if theirs {
                // The first of the commits beside it writes its key.
                let other = bed.world.begin();
                bed.world
                    .write(&other, vec![put(aux(MERK), b"mine", b"theirs")]);
                let made = bed.world.commit(other, &[&tx]);
                assert!(matches!(made, Outcome::Committed(_)), "{made:?}");
            }
            for _ in 0..BESIDE {
                bed.put_elsewhere(&[&tx]);
            }
            assert_eq!(bed.db.commits.kept(), BESIDE + usize::from(theirs));
            match theirs {
                true => assert_eq!(bed.end(tx), Outcome::Conflict),
                false => assert!(matches!(bed.end(tx), Outcome::Committed(_))),
            }
        });
    }
}

/// The mark is on the entry of the node that is met, wherever that entry
/// stands among those of its batch: after the entry of a clear, and after
/// the nodes and the deletes of another Merk. (The write set checks the
/// key of the entry it is told to mark.)
#[test]
fn the_met_node_is_marked_among_the_other_entries_of_its_batch() {
    for by_key in [false, true] {
        bed(Stored::InCell, |bed| {
            let node = bed.node.clone();
            for _ in 0..3 {
                bed.write_elsewhere(&[]);
            }
            let tx = bed.world.begin();
            let (mut batch, kept) = adding(&bed.world, &tx, bed.len);
            assert!(kept.contains(&node));
            // Before the Merk of the met node: another Merk that loses a
            // node and gets one, and a subtree that is cleared.
            let other = bed.world.merk(&tx, OTHER, |items| {
                let first = items.keys().next().cloned().expect("a node");
                items.remove(&first);
                items.insert(b"zz".to_vec(), vec![7; 9]);
            });
            assert!(!other.removed.is_empty() && !other.nodes.is_empty());
            batch.merks.insert(0, other);
            (batch.subtrees).push(OwnedSubtree::Clear(model::subtree(8)));
            let batch = read_now(&tx, &batch);
            bed.rewrite_outside(&[&tx]);

            if by_key {
                // A scope beside another keeps its writes in the write set.
                let first = tx.txn.scope().expect("a scope");
                let beside = tx.txn.scope().expect("a scope");
                assert!(!beside.is_laid_out());
                (bed.world.apply_in(&tx, &beside, &batch)).expect("kept in the write set");
                tx.model.borrow_mut().meet_kept(data(), &node);
                assert_eq!(tx.txn.stats().overlapped, 1);
                beside.merge();
                first.merge();
            } else {
                bed.keep(&tx, &batch, std::slice::from_ref(&node));
            }
            assert_eq!(tx.txn.stats().overlapped, 1, "{by_key}");
            assert_eq!(bed.end(tx), Outcome::Conflict, "{by_key}");
        });
    }
}

/// The mark is the entry's, and goes where the entry goes (CF4): a scope
/// that is rolled back from between takes its own writes out and leaves a
/// met write of another scope met, and the rollback of the scope that made
/// the met write takes the mark with it.
#[test]
fn a_write_that_its_read_met_is_its_scopes_through_the_rollback_of_another() {
    for rolled_back in ["the other scope", "the scope of the met write"] {
        bed(Stored::InCell, |bed| {
            let node = bed.node.clone();
            let tx = bed.world.begin();
            let (batch, writes) = met_by_its_read(bed, &tx);
            let one = tx.txn.scope().expect("a scope");
            let other = tx.txn.scope().expect("a scope");
            // The first scope writes before and after the met write.
            let flat = |key: &[u8]| records(vec![put(aux(7), key, b"1")]);
            (bed.world.apply_in(&tx, &one, &flat(b"before"))).expect("a write");
            (bed.world.apply_in(&tx, &other, &batch)).expect("the write is taken");
            tx.model.borrow_mut().meet_kept(data(), &node);
            (bed.world.apply_in(&tx, &one, &flat(b"after"))).expect("a write");
            let stats = tx.txn.stats();
            assert_eq!((stats.writes, stats.overlapped), (writes + 2, 1));

            if rolled_back == "the other scope" {
                one.rollback();
                let mut model = tx.model.borrow_mut();
                model.writes.pop();
                model.writes.remove(0);
                drop(model);
                let stats = tx.txn.stats();
                assert_eq!((stats.writes, stats.overlapped), (writes, 1));
                other.merge();
                assert_eq!(tx.txn.stats().overlapped, 1);
                assert_eq!(bed.end(tx), Outcome::Conflict, "{rolled_back}");
            } else {
                other.rollback();
                let mut model = tx.model.borrow_mut();
                let last = model.writes.pop().expect("the write after");
                model.writes.truncate(1);
                model.writes.push(last);
                drop(model);
                let stats = tx.txn.stats();
                assert_eq!((stats.writes, stats.overlapped), (2, 0));
                one.merge();
                bed.world.check(&tx);
                assert!(
                    matches!(bed.end(tx), Outcome::Committed(_)),
                    "{rolled_back}"
                );
            }
        });
    }
}

/// A scope that is opened beside another keeps its writes in the write
/// set until it is laid out. A keep it writes is dated as any other: met
/// where a commit wrote the key since the read, and the mark is there
/// after the scope was laid out and merged. With no such commit the value
/// is kept and published.
#[test]
fn a_kept_value_of_a_scope_that_is_laid_out_later_is_dated_at_its_read() {
    for between in [Between::Elsewhere, Between::TheNode] {
        bed(Stored::InCell, |bed| {
            let node = bed.node.clone();
            let tx = bed.world.begin();
            let (old, stated) = shown(&tx, &node);
            let first = tx.txn.scope().expect("a scope");
            (bed.world
                .apply_in(&tx, &first, &records(vec![put(aux(7), b"first", b"1")])))
            .expect("a write");
            let beside = tx.txn.scope().expect("a scope");
            assert!(!beside.is_laid_out());
            // The scope beside reads the nodes itself, through the frames
            // that show the first scope's write.
            let (batch, kept) = adding(&bed.world, &tx, bed.len);
            assert!(kept.contains(&node));
            let batch = batch.through(&beside);
            assert_eq!(batch.merks[0].hints.len(), kept.len());
            let met = match between {
                Between::TheNode => {
                    bed.rewrite_outside(&[&tx]);
                    vec![node.clone()]
                }
                _ => {
                    bed.write_elsewhere(&[&tx]);
                    Vec::new()
                }
            };
            (bed.world.apply_in(&tx, &beside, &batch)).expect("kept in the write set");
            for key in &met {
                tx.model.borrow_mut().meet_kept(data(), key);
            }
            assert_eq!(tx.txn.stats().overlapped, met.len(), "{between:?}");
            first.merge();
            beside.lay_out().expect("it is laid out");
            assert_eq!(tx.txn.stats().overlapped, met.len(), "{between:?}");
            beside.merge();
            assert_eq!(tx.txn.stats().overlapped, met.len(), "{between:?}");
            match met.is_empty() {
                true => {
                    assert_eq!(shown(&tx, &node), (old.clone(), stated));
                    bed.world.check(&tx);
                    assert!(matches!(bed.end(tx), Outcome::Committed(_)), "{between:?}");
                    assert_eq!(committed(&bed.world, &node), (old, stated));
                }
                false => assert_eq!(bed.end(tx), Outcome::Conflict, "{between:?}"),
            }
        });
    }
}

/// Where the transaction cannot say what the commits did since a read, it
/// takes the write to be met (D-098 item 7, rule 4). That is so for a read
/// more than [`KEPT_COMMITS`] commits before the write, for a handle of a
/// lease the transaction has shown no node under since it last ended, and
/// for a handle that another transaction gave out. None of them wrote the
/// node: the conflict is one that need not have been. A cursor that lives
/// through the transaction's end reads the version it was opened at: its
/// reads after the end are answered for where no commit was made since.
#[test]
fn a_kept_value_whose_read_the_transaction_cannot_date_is_met() {
    // As many commits as the transaction keeps the key sets of, and one
    // more.
    for commits in [KEPT_COMMITS, KEPT_COMMITS + 1] {
        bed(Stored::InCell, |bed| {
            let node = bed.node.clone();
            let tx = bed.world.begin();
            let (batch, kept) = adding(&bed.world, &tx, bed.len);
            let batch = read_now(&tx, &batch);
            let (old, stated) = shown(&tx, &node);
            for _ in 0..commits {
                bed.write_elsewhere(&[&tx]);
            }
            if commits <= KEPT_COMMITS {
                bed.keep(&tx, &batch, &[]);
                assert!(matches!(bed.end(tx), Outcome::Committed(_)), "{commits}");
                assert_eq!(committed(&bed.world, &node), (old, stated));
            } else {
                // Every keep of the batch is of that read.
                bed.keep(&tx, &batch, &kept);
                assert_eq!(bed.end(tx), Outcome::Conflict, "{commits}");
            }
        });
    }

    // A handle from before a rollback of the transaction: its handles
    // ended with it, and it has forgotten when it gave them.
    bed(Stored::InCell, |bed| {
        let tx = bed.world.begin();
        let (batch, kept) = adding(&bed.world, &tx, bed.len);
        let batch = read_now(&tx, &batch);
        bed.committer.rollback(&tx.txn).expect("no scope is open");
        bed.keep(&tx, &batch, &kept);
        assert_eq!(bed.end(tx), Outcome::Conflict);
    });

    // A handle from before a commit of the transaction that committed
    // nothing: it ends the transaction all the same.
    bed(Stored::InCell, |bed| {
        let tx = bed.world.begin();
        let (batch, kept) = adding(&bed.world, &tx, bed.len);
        let batch = read_now(&tx, &batch);
        let ended = bed.db.commit(bed.versions, &tx.txn).expect("a commit");
        assert_eq!(ended, Outcome::Empty);
        bed.keep(&tx, &batch, &kept);
        assert_eq!(bed.end(tx), Outcome::Conflict);
    });

    // A cursor that was opened before such a commit shows a node after
    // it: that is a read of now, and with no commit since it a keep by its
    // handle is answered for.
    bed(Stored::InCell, |bed| {
        let node = bed.node.clone();
        let tx = bed.world.begin();
        let (old, stated) = shown(&tx, &node);
        let (batch, _) = adding(&bed.world, &tx, bed.len);
        let mut cursor = tx.txn.merk_cursor(model::subtree(MERK)).expect("a cursor");
        let ended = bed.db.commit(bed.versions, &tx.txn).expect("a commit");
        assert_eq!(ended, Outcome::Empty);
        let batch = batch.reading(|_, key| {
            cursor.seek(key).expect("a seek");
            assert_eq!(cursor.key(), Some(key));
            cursor.node(handle_of).expect("a read")
        });
        drop(cursor);
        assert!(tx.txn.watches());
        bed.keep(&tx, &batch, &[]);
        assert!(matches!(bed.end(tx), Outcome::Committed(_)));
        assert_eq!(committed(&bed.world, &node), (old, stated));
    });

    // The transaction's lock is not held while a cursor reads the node of
    // its own version, so another thread can end the transaction between
    // the two halves of the cursor's read. The rollback inside the last
    // read stands in for that thread: it ends the transaction after the
    // cursor has found the node and before it hands the handle over and
    // tells the transaction. A keep by that handle is answered for.
    bed(Stored::InCell, |bed| {
        let node = bed.node.clone();
        let tx = bed.world.begin();
        let (old, stated) = shown(&tx, &node);
        let (batch, kept) = adding(&bed.world, &tx, bed.len);
        let mut cursor = tx.txn.merk_cursor(model::subtree(MERK)).expect("a cursor");
        let mut left = kept.len();
        let batch = batch.reading(|_, key| {
            cursor.seek(key).expect("a seek");
            assert_eq!(cursor.key(), Some(key));
            left -= 1;
            let read = cursor.node(|view| {
                if left == 0 {
                    bed.committer.rollback(&tx.txn).expect("no scope is open");
                }
                handle_of(view)
            });
            read.expect("a read")
        });
        drop(cursor);
        assert_eq!(left, 0);
        assert!(tx.txn.watches());
        bed.keep(&tx, &batch, &[]);
        assert!(matches!(bed.end(tx), Outcome::Committed(_)));
        assert_eq!(committed(&bed.world, &node), (old, stated));
    });

    // With a commit since, the cursor still shows the version it was
    // opened at, and the transaction did not watch in between: every keep
    // by its handles is met.
    bed(Stored::InCell, |bed| {
        let tx = bed.world.begin();
        let (batch, kept) = adding(&bed.world, &tx, bed.len);
        let mut cursor = tx.txn.merk_cursor(model::subtree(MERK)).expect("a cursor");
        let ended = bed.db.commit(bed.versions, &tx.txn).expect("a commit");
        assert_eq!(ended, Outcome::Empty);
        bed.rewrite_outside(&[&tx]);
        let batch = batch.reading(|_, key| {
            cursor.seek(key).expect("a seek");
            assert_eq!(cursor.key(), Some(key));
            cursor.node(handle_of).expect("a read")
        });
        drop(cursor);
        bed.keep(&tx, &batch, &kept);
        assert_eq!(bed.end(tx), Outcome::Conflict);
    });

    // A handle of another transaction.
    bed(Stored::InCell, |bed| {
        let (tx, other) = (bed.world.begin(), bed.world.begin());
        let (batch, kept) = adding(&bed.world, &tx, bed.len);
        let batch = read_now(&other, &batch);
        bed.keep(&tx, &batch, &kept);
        assert_eq!(bed.end(tx), Outcome::Conflict);
        drop(other);
    });
}

/// What the rule has a transaction hold. It watches the key sets of the
/// commits from the first operation that shows it a node, as it does from
/// its first write, and until it ends. A transaction that reads flat
/// records, by key or under a cursor, watches nothing, as before.
#[test]
fn a_transaction_watches_from_the_first_node_it_is_shown() {
    bed(Stored::InCell, |bed| {
        let node = bed.node.clone();
        bed.world
            .write_outside(&records(vec![put(aux(7), b"flat", b"1")]), &[]);
        let kept = || bed.db.commits.kept();

        // Flat records only: no watch, and no key set is kept for it.
        let flat = bed.world.begin();
        let mut out = Vec::new();
        assert!(flat.txn.get(aux(7), b"flat", &mut out).expect("a read"));
        assert!((flat.txn.committed_get(aux(7), b"flat", &mut out)).expect("a read"));
        let mut cursor = flat.txn.cursor(aux(7));
        cursor.seek_to_first().expect("a seek");
        assert_eq!(cursor.key(), Some(&b"flat"[..]));
        drop(cursor);
        assert!(!flat.txn.watches());
        for _ in 0..3 {
            bed.write_elsewhere(&[&flat]);
        }
        assert_eq!(kept(), 1, "the newest, which is kept in any case");

        // A node is shown, and no handle is taken: the watch starts, for
        // the handle that the read could have given.
        let tx = bed.world.begin();
        let value = |view: Option<NodeView<'_>>| view.map(|view| value_of(&view));
        (tx.txn.node(model::subtree(MERK), &node, value)).expect("a read");
        assert!(tx.txn.watches());
        for _ in 0..3 {
            bed.write_elsewhere(&[&tx]);
        }
        assert_eq!(kept(), 3, "what the transaction has not taken in");
        // Its next operation takes them in.
        (tx.txn.node(model::subtree(MERK), &node, value)).expect("a read");
        assert_eq!(kept(), 1);

        // The other reads that show a node start it too.
        let reads: [&dyn Fn(&WriteTxn<'_>); 2] = [
            &|txn| drop((txn.committed_node(model::subtree(MERK), &node, value)).expect("a read")),
            &|txn| drop(txn.merk_cursor(model::subtree(MERK)).expect("a cursor")),
        ];
        for read in reads {
            let other = bed.world.begin();
            assert!(!other.txn.watches());
            read(&other.txn);
            assert!(other.txn.watches());
            // What was shown is remembered, also of a cursor that is
            // gone: a read that shows nothing leaves the watch then.
            let none = (other.txn).node(model::subtree(MERK), b"no node", value);
            assert_eq!(none.expect("a read"), None);
            assert!(other.txn.watches());
        }

        // A read that shows no node dates nothing and leaves no watch: a
        // key with no node, and a handle that is not the transaction's.
        let none = bed.world.begin();
        let merk = model::subtree(MERK);
        assert_eq!(
            none.txn.node(merk, b"no node", value).expect("a read"),
            None
        );
        assert!(!none.txn.watches());
        let committed = none.txn.committed_node(merk, b"no node", value);
        assert_eq!(committed.expect("a read"), None);
        assert!(!none.txn.watches());
        let foreign = (tx.txn.node(merk, &node, handle_of)).expect("a read");
        let child = none
            .txn
            .child(merk, foreign.expect("a handle"), Side::Left, value);
        assert_eq!(child.expect("a read"), None);
        assert!(!none.txn.watches());
        for _ in 0..2 {
            bed.write_elsewhere(&[&tx, &none]);
        }
        (tx.txn.node(merk, &node, value)).expect("a read");
        assert_eq!(
            kept(),
            1,
            "nothing is kept for a transaction that saw no node"
        );

        // A read that is refused leaves no watch either: a scope keeps the
        // claims of a batch that was refused, and the read of another
        // scope meets them.
        let claimed = bed.world.begin();
        let (batch, _) = adding(&bed.world, &claimed, bed.len);
        let first = claimed.txn.scope().expect("a scope");
        let second = claimed.txn.scope().expect("a scope");
        let refused = batch.with(|batch| second.apply(batch));
        assert!(no_read(&refused, MERK), "{refused:?}");
        assert!(!claimed.txn.watches());
        let read = first.node(merk, &node, value);
        assert!(
            matches!(read, Err(TxnError::ScopeOverlap { .. })),
            "{read:?}"
        );
        assert!(!claimed.txn.watches());
        second.merge();
        first.merge();

        // A cursor that lives through the transaction's end starts the
        // watch anew when it shows a node, and not when it shows none: it
        // stands on a key the transaction put and took back.
        let ended = bed.world.begin();
        let mut cursor = ended.txn.merk_cursor(merk).expect("a cursor");
        let (batch, _) = adding(&bed.world, &ended, bed.len);
        let added = (batch.merks[0].nodes.iter()).find(|(.., stated)| *stated);
        let added = added.expect("a node is added").0.clone();
        bed.world.apply(&ended, &batch).expect("the write is taken");
        cursor.seek(&added).expect("a seek");
        assert_eq!(cursor.key(), Some(&added[..]));
        bed.committer
            .rollback(&ended.txn)
            .expect("no scope is open");
        ended.model.borrow_mut().writes.clear();
        assert!(!ended.txn.watches());
        assert_eq!(cursor.node(value).expect("a read"), None);
        assert!(!ended.txn.watches());
        cursor.seek(&node).expect("a seek");
        assert!(cursor.node(value).expect("a read").is_some());
        assert!(ended.txn.watches());
        drop(cursor);
        bed.committer
            .rollback(&ended.txn)
            .expect("no scope is open");
        assert!(!ended.txn.watches());

        // The transaction can forget the cursor's lease while the cursor
        // reads: another operation of it takes in more commits of others
        // than it keeps key sets of, between the two halves of a read
        // that shows no node. The read made inside the callback stands in
        // for the thread that runs that operation. No watch is left.
        let evicted = bed.world.begin();
        let mark = evicted.txn.mark();
        let mut cursor = evicted.txn.merk_cursor(merk).expect("a cursor");
        let (batch, _) = adding(&bed.world, &evicted, bed.len);
        let added = (batch.merks[0].nodes.iter()).find(|(.., stated)| *stated);
        let added = added.expect("a node is added").0.clone();
        bed.world
            .apply(&evicted, &batch)
            .expect("the write is taken");
        cursor.seek(&added).expect("a seek");
        assert!(evicted.txn.rewind(mark));
        evicted.model.borrow_mut().writes.clear();
        assert!(evicted.txn.watches());
        let shown = cursor.node(|view| {
            for _ in 0..=KEPT_COMMITS {
                bed.write_elsewhere(&[&evicted]);
            }
            let mut out = Vec::new();
            assert!((evicted.txn.get(aux(7), b"flat", &mut out)).expect("a read"));
            view.map(|view| value_of(&view))
        });
        assert_eq!(shown.expect("a read"), None);
        assert!(!evicted.txn.watches());
        drop(cursor);

        // A commit of nothing ends the watch as any commit does.
        let empty = bed.world.begin();
        (empty.txn.node(merk, &node, value)).expect("a read");
        assert!(empty.txn.watches());
        let ended = bed.db.commit(bed.versions, &empty.txn).expect("a commit");
        assert_eq!(ended, Outcome::Empty);
        assert!(!empty.txn.watches());

        // The transaction's end ends the watch, and what it remembers.
        bed.committer.rollback(&tx.txn).expect("no scope is open");
        assert!(!tx.txn.watches());
        bed.write_elsewhere(&[&tx]);
        assert_eq!(kept(), 1);
    });
}

/// The keys of the two-thread schedule's Merk, which never change: its
/// commits change values, so every commit rewrites the nodes from the one
/// it changes up to the root, all but the first with their values kept.
const KEYS: u8 = 15;

/// What a writer of the schedule read: the value of every node of a Merk
/// and the handle it was shown with.
struct Seen {
    items: Items,
    handles: BTreeMap<Vec<u8>, NodeHandle>,
}

/// Reads every node of the Merk `name` through `txn`, in the order of the
/// keys, and checks that each is shown with the hash of the value it is
/// shown with. `during` is called after each node is read, with the place
/// of its key.
///
/// Each node is read at the version that is the latest then: where a
/// commit is published during the read, what is returned is of two
/// versions.
fn see_during(txn: &WriteTxn<'_>, name: u8, mut during: impl FnMut(u8)) -> Seen {
    let mut seen = Seen {
        items: Items::new(),
        handles: BTreeMap::new(),
    };
    for at in 0..KEYS {
        let key = key(at);
        let read = |view: Option<NodeView<'_>>| {
            let view = view.expect("the node is there");
            (value_of(&view), *view.value_hash(), view.handle())
        };
        let (value, hash, handle) = (txn.node(model::subtree(name), &key, read)).expect("a read");
        assert_eq!(hash, hash_of(&[&value]), "{key:?} of the Merk {name}");
        seen.handles
            .extend(handle.map(|handle| (key.clone(), handle)));
        seen.items.insert(key, value);
        during(at);
    }
    seen
}

/// [`see_during`] with nothing done between the nodes: for a look at what
/// is published, which makes no hash from what it reads.
fn see(txn: &WriteTxn<'_>, name: u8) -> Seen {
    see_during(txn, name, |_| ())
}

/// The Merk `name` as one version has it: [`see_during`], made again
/// while a commit was published during it. This is the read of a writer
/// of the schedule, which makes the hashes it states from the values it
/// read ([`change`]).
fn see_at_one_version(
    versions: &Versions,
    txn: &WriteTxn<'_>,
    name: u8,
    mut during: impl FnMut(u8),
) -> Seen {
    loop {
        let before = versions.latest();
        let seen = see_during(txn, name, &mut during);
        if versions.latest() == before {
            break seen;
        }
    }
}

/// The batch that gives the node `key` of the Merk `name` the value
/// `value`, made for what `seen` says the Merk holds: the node with its
/// value stated, and every node above it with its value kept, by the
/// handle it was seen with. Returns it with the keys of the nodes that
/// keep their values.
fn change(seen: &Seen, name: u8, key: &[u8], value: Vec<u8>) -> (OwnedBatch, Vec<Vec<u8>>) {
    let view: BTreeMap<Vec<u8>, Val> = (model::records(&seen.items).1.into_iter())
        .map(|(key, node)| (key, Val::Node(node)))
        .collect();
    let mut items = seen.items.clone();
    items.insert(key.to_vec(), value);
    let mut merk = OwnedMerk::change(model::subtree(name), &view, &items);
    assert!(merk.root.is_none() && merk.removed.is_empty());
    let kept: Vec<Vec<u8>> = (merk.nodes.iter())
        .filter(|(_, _, stated)| !stated)
        .map(|(key, ..)| key.clone())
        .collect();
    merk.hints = (kept.iter())
        .map(|key| (key.clone(), seen.handles[key]))
        .collect();
    let batch = OwnedBatch {
        merks: vec![merk],
        ..OwnedBatch::default()
    };
    (batch, kept)
}

/// Gives another thread a moment to publish a commit: until the latest
/// version has moved, or for a bounded number of turns.
fn wait_for_a_commit(versions: &Versions) {
    let before = versions.latest();
    for _ in 0..64 {
        if versions.latest() != before {
            break;
        }
        thread::yield_now();
    }
}

/// Makes the two Merks of a schedule, each with a value from `value` under
/// every key, and returns what the Merk [`MERK`] holds.
fn make_merks(
    db: &Db,
    versions: &Versions,
    path: WritePath<'_>,
    mut value: impl FnMut() -> Vec<u8>,
) -> Items {
    let mut made = Vec::new();
    for name in [MERK, OTHER] {
        let txn = path.begin();
        let items: Items = (0..KEYS).map(|at| (key(at), value())).collect();
        let merk = OwnedMerk::change(model::subtree(name), &BTreeMap::new(), &items);
        let records = World::root_record(&merk).into_iter().collect();
        let batch = OwnedBatch {
            merks: vec![merk],
            records,
            ..OwnedBatch::default()
        };
        let scope = txn.scope().expect("its own transaction");
        (batch.with(|batch| scope.apply(batch))).expect("the Merk is made");
        scope.merge();
        let committed = db.commit(versions, &txn).expect("a commit");
        assert!(matches!(committed, Outcome::Committed(_)));
        made.push(items);
    }
    made.swap_remove(0)
}

/// The place of the key `node` among the schedule's keys.
fn place(node: &[u8]) -> u8 {
    (0..KEYS).find(|at| key(*at) == node).expect("a key")
}

/// Writes the node in the place `at` of the Merk `name` with the value
/// `value` through `txn`, as a writer does whose view of the Merk is
/// `seen`, and commits. Returns whether the commit was made, with the keys
/// of the nodes it wrote and of those among them that kept their values.
fn write_as_seen(
    db: &Db,
    versions: &Versions,
    txn: &WriteTxn<'_>,
    seen: &Seen,
    (name, at, value): (u8, u8, Vec<u8>),
) -> (bool, Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let (batch, kept) = change(seen, name, &key(at), value);
    let scope = txn.scope().expect("its own transaction");
    (batch.with(|batch| scope.apply(batch))).expect("the write is taken");
    scope.merge();
    let wrote = (batch.merks[0].nodes.iter())
        .map(|(key, ..)| key.clone())
        .collect();
    let made = match db.commit(versions, txn).expect("a commit") {
        Outcome::Committed(_) => true,
        Outcome::Conflict => false,
        Outcome::Empty => panic!("the transaction wrote"),
    };
    (made, wrote, kept)
}

/// The cell of the node `node` of the Merk [`MERK`], as it is committed.
fn cell_of(path: WritePath<'_>, node: &[u8]) -> Place {
    let read = |view: Option<NodeView<'_>>| view.expect("the node is there").handle();
    let handle = (path.begin().node(model::subtree(MERK), node, read)).expect("a read");
    Place::Cell(handle.expect("a committed node").address())
}

/// A place among the schedule's keys that is not `root`, the place of the
/// key of the Merks' root.
fn below_the_root(rng: &mut Rng, root: u8) -> u8 {
    let at = rng.below(usize::from(KEYS) - 1) as u8;
    at + u8::from(at >= root)
}

/// One transaction of the schedule: it reads the Merk `name`, lets
/// `between` happen, writes the node `key` with a new value and commits.
/// Returns whether the commit was made, with the keys of the nodes it
/// wrote and of those among them that kept their values.
///
/// The writer makes the hashes of the nodes it writes from the values it
/// read, and it stays within what the conflict rule answers for, so that
/// whatever it publishes is the Merk of the values that are stored:
///
/// - **Its view is of one version** ([`see_at_one_version`]). From values
///   of two versions it would state, in a node of the newer one, the hash
///   of a child as the older one had it
///   (`a_view_of_two_versions_publishes_a_link_hash_of_the_older_one`). A
///   Merk has no such view: it reads the links of a node with the node.
/// - **It does not change the value of the root.** A node whose value is
///   stated states its links with it, and nothing of it is dated at a
///   read (D-155 item 3): after a commit that changed a child, the root
///   is published with the link hashes of the writer's read
///   (`a_stated_value_is_published_with_the_link_hashes_of_its_read`).
///   Every other node has the root above it, which the writer keeps the
///   value of. Every commit of the schedule to the Merk writes the root,
///   so one since the writer's read is then a conflict.
fn rewrite(
    db: &Db,
    versions: &Versions,
    path: WritePath<'_>,
    write: (u8, u8, Vec<u8>),
    between: impl FnOnce(),
) -> (bool, Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let txn = path.begin();
    let seen = see_at_one_version(versions, &txn, write.0, |_| ());
    between();
    let done = write_as_seen(db, versions, &txn, &seen, write);
    assert!(!done.2.is_empty(), "the root is above the node, and kept");
    // Whatever is published has the hash of its value under every key.
    for name in [MERK, OTHER] {
        see(&path.begin(), name);
    }
    done
}

/// What the rule of kept values does not date, pinned (D-155 item 3;
/// `transactions.md` case A2, for a Merk node). A node that is written with
/// its value stated is written whole, its links with it, and names no
/// read: the conflict rule looks from the write (CF2). So a writer that
/// reads the root, and states a new value for it after the commit of
/// another transaction changed a child, is in no conflict, and the root it
/// publishes has the hash of the child as the writer read it. A
/// whole-record put of RocksDB does the same.
///
/// The physical checker reports that link and nothing else. The next
/// commit to the Merk by a writer that reads it then writes the hash of
/// the child that is stored.
#[test]
fn a_stated_value_is_published_with_the_link_hashes_of_its_read() {
    let (db, versions) = Db::new(96);
    let path = db.path(&versions);
    let mut rng = Rng::new(0x1157_a7ed);
    let items = make_merks(&db, &versions, path, || model::value(&mut rng, 24));
    let (root, records) = model::records(&items);
    let root = root.expect("a root");
    let (side, child) = ([Side::Left, Side::Right].into_iter())
        .find_map(|side| Some((side, records[&root].link(side)?.key.clone())))
        .expect("the root has a child");

    // The writer reads the Merk. Another transaction changes the child,
    // which writes the root again with the child's new hash.
    let txn = path.begin();
    let seen = see(&txn, MERK);
    let theirs = (MERK, place(&child), model::value(&mut rng, 24));
    let (made, wrote, kept) = rewrite(&db, &versions, path, theirs, || ());
    assert!(made);
    assert_eq!(wrote.len(), 2, "the child and the root");
    assert_eq!(kept, std::slice::from_ref(&root));

    // The writer states a new value for the root. It keeps no value, and
    // its first write of the root is after the other commit: no conflict.
    let ours = (MERK, place(&root), model::value(&mut rng, 24));
    let (made, wrote, kept) = write_as_seen(&db, &versions, &txn, &seen, ours);
    assert!(made, "the commit is taken");
    assert_eq!(wrote, std::slice::from_ref(&root));
    assert!(kept.is_empty());

    // The root is published with the hash of the child as the writer read
    // it, which is not the child's.
    let report = db.verify_still();
    let found = db.unsound(&report);
    assert_eq!((found.len(), report.more_findings), (1, 0), "{report}");
    assert_eq!(
        found[0].rule,
        Rule::Hash(HashField::Child(side)),
        "{report}"
    );
    assert_eq!(found[0].place, cell_of(path, &root), "{report}");

    // A writer that reads the Merk now writes the root with the hashes of
    // the children that are stored.
    let next = (MERK, place(&child), model::value(&mut rng, 24));
    let (made, ..) = rewrite(&db, &versions, path, next, || ());
    assert!(made);
    let report = db.verify_still();
    assert!(
        db.unsound(&report).is_empty() && report.more_findings == 0,
        "{report}"
    );
}

/// What a writer of the schedule must not do, and how it does not. It
/// makes the link hashes it states from the values it read ([`change`]).
/// Where a commit of another transaction is published during its read, the
/// values are of two versions: here the first node as it was before that
/// commit changed it, and every other node as it is after. The writer
/// changes the other child of the node above the first. It writes the node
/// above with its value kept, and with the hash of the first node as it
/// read it for the link to it, which is the hash of the older version. No
/// node it keeps the value of was read before that commit, so its own
/// commit is in no conflict, and the physical checker reports the link.
/// This is the writer's doing. A Merk reads the links of a node with the
/// node. The engine dates a kept value at the read its write names, and
/// this write names no read of the first node: it states that node's
/// hash in a link of the node above, and links are not dated.
///
/// The read of the schedule is made again while a commit was published
/// during it ([`see_at_one_version`]), and what its writer then publishes
/// is the Merk of the values that are stored.
#[test]
fn a_view_of_two_versions_publishes_a_link_hash_of_the_older_one() {
    let (db, versions) = Db::new(96);
    let path = db.path(&versions);
    let mut rng = Rng::new(0x2057_a7ed);
    let items = make_merks(&db, &versions, path, || model::value(&mut rng, 24));
    let records = model::records(&items).1;
    // The node that is read first, the node above it with the side it is
    // on, and the child on the other side, which the writer changes.
    let first = key(0);
    let (above, side, other) = (records.iter())
        .find_map(|(node, record)| {
            let (side, across) = [(Side::Left, Side::Right), (Side::Right, Side::Left)]
                .into_iter()
                .find(|(side, _)| record.link(*side).is_some_and(|link| link.key == first))?;
            Some((node.clone(), side, record.link(across)?.key.clone()))
        })
        .expect("the node above the first has another child");
    let other = place(&other);
    let theirs = |rng: &mut Rng| {
        let value = model::value(rng, 24);
        let (made, ..) = rewrite(&db, &versions, path, (MERK, 0, value.clone()), || ());
        assert!(made);
        value
    };

    // A read with a commit after its first node, as it is.
    let txn = path.begin();
    let seen = see_during(&txn, MERK, |at| {
        if at == 0 {
            theirs(&mut rng);
        }
    });
    assert_eq!(seen.items[&first], items[&first], "as it was before");
    let ours = (MERK, other, model::value(&mut rng, 24));
    let (made, _, kept) = write_as_seen(&db, &versions, &txn, &seen, ours);
    assert!(made, "the commit is taken");
    assert!(kept.contains(&above) && !kept.contains(&first));
    let report = db.verify_still();
    let found = db.unsound(&report);
    assert_eq!((found.len(), report.more_findings), (1, 0), "{report}");
    assert_eq!(
        found[0].rule,
        Rule::Hash(HashField::Child(side)),
        "{report}"
    );
    assert_eq!(found[0].place, cell_of(path, &above), "{report}");

    // The read of the schedule, with a commit after the first node of its
    // first pass: it is made a second time, and is of one version.
    let txn = path.begin();
    let (mut passes, mut theirs_now) = (0, Vec::new());
    let seen = see_at_one_version(&versions, &txn, MERK, |at| {
        if at == 0 {
            passes += 1;
            if passes == 1 {
                theirs_now = theirs(&mut rng);
            }
        }
    });
    assert_eq!(passes, 2);
    assert_eq!(seen.items[&first], theirs_now, "as it is now");
    let ours = (MERK, other, model::value(&mut rng, 24));
    let (made, ..) = write_as_seen(&db, &versions, &txn, &seen, ours);
    assert!(made);
    let report = db.verify_still();
    assert!(
        db.unsound(&report).is_empty() && report.more_findings == 0,
        "{report}"
    );
}

/// A writer on one thread reads the nodes of a Merk and later writes them
/// again with their values kept, while a second thread commits to the
/// same keys and to others. After every commit, every node has the hash
/// of its value, and what is left at the end is checked whole, with the
/// hash in every link.
///
/// The first part is interleaved by construction: the second thread makes
/// the commits the writer asks it for, between the writer's read and its
/// write, and each is checked before the next is made. So the outcome of
/// each of the writer's commits is known: a conflict exactly where one of
/// those commits wrote a node whose value the writer keeps.
///
/// In the second part the two run freely, each waiting a little between
/// its read and its write for the other to commit. Which commits conflict
/// is the scheduler's to say there, and only what is published is
/// checked: by each thread after each of its commits, in the state that
/// is the latest then.
#[test]
fn kept_values_on_two_threads_are_never_published_under_a_hash_of_another_value() {
    const ROUNDS: usize = if cfg!(miri) { 6 } else { 150 };
    const FREE: usize = if cfg!(miri) { 4 } else { 120 };
    /// What the writer asks of the second thread: a commit to a node of
    /// one of the two Merks, or to run by itself until it is told to stop.
    enum Ask {
        Commit(u8, u8, Vec<u8>),
        Free,
    }
    /// Tells the second thread to stop when the writer is done, also where
    /// the writer is done by a failed assertion.
    struct Stop<'a>(&'a AtomicBool);
    impl Drop for Stop<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }
    let (db, versions) = Db::new(96);
    let path = db.path(&versions);
    let mut rng = Rng::new(0x0f1a_57ed);
    let value = |rng: &mut Rng| {
        let len = [9, 24, 700][rng.below(3)];
        model::value(rng, len)
    };
    // The two Merks, with every key.
    let items = make_merks(&db, &versions, path, || value(&mut rng));
    // The two Merks have the same keys, and so the same root.
    let root = place(&model::records(&items).0.expect("a root"));

    let (asks, asked) = mpsc::channel::<Ask>();
    let (answers, answered) = mpsc::channel::<Vec<Vec<u8>>>();
    let stop = AtomicBool::new(false);
    let (mut conflicts, mut beside, mut alone) = (0, 0, 0);
    thread::scope(|threads| {
        let (db, versions, stop) = (&*db, &versions, &stop);
        let second = threads.spawn(move || {
            let mut rng = Rng::new(0x05ec_017d);
            let mut made = 0usize;
            while let Ok(ask) = asked.recv() {
                match ask {
                    Ask::Commit(name, at, value) => {
                        // The writer waits: nothing is committed beside
                        // this, and it is made.
                        let (committed, wrote, _) =
                            rewrite(db, versions, path, (name, at, value), || ());
                        assert!(committed, "a commit that nothing is beside");
                        let wrote = if name == MERK { wrote } else { Vec::new() };
                        answers.send(wrote).expect("the writer waits");
                    }
                    Ask::Free => {
                        while !stop.load(Ordering::Relaxed) {
                            let name = [MERK, MERK, OTHER][rng.below(3)];
                            let at = below_the_root(&mut rng, root);
                            let len = [9, 24, 700][rng.below(3)];
                            let value = model::value(&mut rng, len);
                            let wait = || wait_for_a_commit(versions);
                            let (committed, ..) =
                                rewrite(db, versions, path, (name, at, value), wait);
                            made += usize::from(committed);
                        }
                    }
                }
            }
            made
        });

        // The part that is interleaved by construction.
        for round in 0..ROUNDS {
            let at = below_the_root(&mut rng, root);
            let new = value(&mut rng);
            let commits = rng.below(4);
            let same = rng.chance(50);
            let mut theirs: Vec<(u8, u8, Vec<u8>)> = (0..commits)
                .map(|_| {
                    let name = if same { MERK } else { OTHER };
                    (name, below_the_root(&mut rng, root), value(&mut rng))
                })
                .collect();
            let mut written = BTreeSet::new();
            let between = || {
                for (name, at, value) in theirs.drain(..) {
                    asks.send(Ask::Commit(name, at, value)).expect("it runs");
                    written.extend(answered.recv().expect("it answers"));
                }
            };
            let (made, _, kept) = rewrite(db, versions, path, (MERK, at, new), between);
            let met = kept.iter().any(|key| written.contains(key));
            assert_eq!(
                made, !met,
                "round {round}: {kept:?} kept, {written:?} written"
            );
            conflicts += usize::from(met);
            beside += usize::from(!met && commits > 0);
            alone += usize::from(commits == 0);
        }

        // The part in which the two run freely.
        let stopping = Stop(stop);
        asks.send(Ask::Free).expect("it runs");
        let mut made = 0usize;
        for _ in 0..FREE {
            let at = below_the_root(&mut rng, root);
            let new = value(&mut rng);
            let wait = || wait_for_a_commit(versions);
            let (committed, ..) = rewrite(db, versions, path, (MERK, at, new), wait);
            made += usize::from(committed);
        }
        drop(stopping);
        drop(asks);
        let theirs = second.join().expect("the second thread");
        assert!(made + theirs > 0, "a commit is made in the free part");
    });
    if !cfg!(miri) {
        assert!(conflicts > 10, "{conflicts} conflicts");
        assert!(beside > 10, "{beside} commits beside others");
        assert!(alone > 10, "{alone} commits with none beside");
    }
    for name in [MERK, OTHER] {
        see(&path.begin(), name);
    }
    db.check_trees(&versions, versions.latest());
    assert_eq!((db.pool.census().private, db.private_ids()), (0, 0));
    assert!(db.flush_errors.lock().expect("the errors").is_empty());
}
