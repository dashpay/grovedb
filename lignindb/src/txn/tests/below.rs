//! What becomes of the index of what a transaction wrote below a Merk
//! (`txn::below`) at each thing that changes it: the set coming to have an
//! entry under a subtree, an enter, a move, a clear, a drop, a batch that
//! is refused, a rollback to a mark, a scope taken out, frames laid out
//! again, and a transaction given up. The tests read the index of the
//! transaction's frames, by the names of the subtrees, after the steps
//! they say something of, and count the entries of the directory that
//! were opened for it (`TxnStats::below_opened`). Every answer that the
//! index gives is held to what every written subtree says
//! (`Frames::below`), and the tests count those comparisons.

use std::collections::{BTreeMap, HashMap};

use lignin_tree::directory::MerkId;

use super::{
    leaves::{sub, Changes},
    model::{handle_of, OwnedBatch, OwnedSubtree},
    store::{Db, Outcome, ENTERED},
    subtrees::{aux, Held},
    *,
};
use crate::txn::frames::held::{compared, Compared};

const TOP: u8 = ENTERED + 30;
const A: u8 = ENTERED + 31;
const B: u8 = ENTERED + 32;
const C: u8 = ENTERED + 33;
const D: u8 = ENTERED + 34;
const E: u8 = ENTERED + 35;
/// The prefixes that a move brings `B` and `D` to.
const B2: u8 = ENTERED + 36;
const D2: u8 = ENTERED + 37;
/// Subtrees with no host and one node each: a clear of one is a batch
/// that takes a Merk with a node, and nothing is below it.
const L1: u8 = ENTERED + 40;
const L2: u8 = ENTERED + 41;
const L3: u8 = ENTERED + 42;
const L4: u8 = ENTERED + 43;
const L5: u8 = ENTERED + 44;
const X: u8 = ENTERED + 45;
const NAMES: [u8; 14] = [TOP, A, B, C, D, E, B2, D2, L1, L2, L3, L4, L5, X];

/// What the index holds, by the names of the subtrees.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Index {
    /// For each subtree that was asked, the Merks it is listed under, the
    /// outermost first.
    listed: BTreeMap<u8, Vec<u8>>,
    /// The subtrees to ask again.
    again: Vec<u8>,
    /// How many first writes of the set were taken.
    taken: u64,
}

fn index(listed: &[(u8, &[u8])], again: &[u8], taken: u64) -> Option<Index> {
    Some(Index {
        listed: (listed.iter())
            .map(|(name, above)| (*name, above.to_vec()))
            .collect(),
        again: again.to_vec(),
        taken,
    })
}

fn held(calls: u64, asked: u64, found: u64, subtrees: u64) -> Compared {
    Compared {
        calls,
        asked,
        found,
        subtrees,
    }
}

/// A transaction over subtrees that are entered under each other, with
/// what the test knows of them.
struct Grove<'w, 's> {
    world: &'w World<'s>,
    tx: Tx<'s>,
    held: Held,
    /// The subtrees as the last commit left them.
    committed: Held,
    /// The subtree that each id was seen under last.
    names: HashMap<MerkId, u8>,
    stamp: u8,
}

impl<'w, 's> Grove<'w, 's> {
    fn new(world: &'w World<'s>) -> Self {
        Self {
            world,
            tx: world.begin(),
            held: Held::default(),
            committed: Held::default(),
            names: HashMap::new(),
            stamp: 0,
        }
    }

    /// The batch that does `subtrees` with the change sets of `changes`,
    /// each Merk with the nodes that host it written again, as the
    /// element of a tree is written wherever its Merk changed.
    fn batch(&mut self, subtrees: Vec<OwnedSubtree>, changes: Changes) -> OwnedBatch {
        self.stamp += 1;
        OwnedBatch {
            merks: changes.merks(&mut self.held, self.stamp),
            subtrees,
            ..OwnedBatch::default()
        }
    }

    /// Applies such a batch through a scope that merges.
    fn apply(&mut self, subtrees: Vec<OwnedSubtree>, changes: Changes) {
        let batch = self.batch(subtrees, changes);
        let scope = self.tx.txn.scope().expect("a scope");
        let applied = batch.through(&scope).with(|batch| scope.apply(batch));
        applied.expect("the batch is applied");
        scope.merge();
        self.learn();
    }

    /// Enters `name` with no node, under a node of `host` that the batch
    /// writes, or with no host.
    fn enter(&mut self, name: u8, host: Option<u8>) {
        let mut changes = Changes::default();
        let host = host.map(|host| (host, vec![b'h', name]));
        if let Some((host, key)) = host.clone() {
            changes.hosted.insert((host, key.clone()), name);
            changes.edit(host, move |items| drop(items.insert(key, b"tree".to_vec())));
        }
        let at = host.clone().map(|(host, key)| (sub(host), key, 0));
        self.apply(vec![OwnedSubtree::Enter(sub(name), at)], changes);
        self.held.live.insert(name, (host, BTreeMap::new()));
    }

    /// The change that writes the node `node` into the Merk `name`.
    fn node(name: u8, node: u8) -> Changes {
        let mut changes = Changes::default();
        changes.edit(name, move |items| {
            items.insert(vec![b'n', node], vec![node; 12]);
        });
        changes
    }

    /// Writes the node `node` into the Merk `name`: the set has an entry
    /// under `name` and under every subtree above it from here on.
    fn write(&mut self, name: u8, node: u8) {
        self.apply(Vec::new(), Self::node(name, node));
    }

    /// Drops `name`, with the node that hosts it.
    fn drop(&mut self, name: u8) {
        let mut changes = Changes::default();
        if let Some((host, key)) = self.held.live[&name].0.clone() {
            changes.edit(host, move |items| drop(items.remove(&key)));
        }
        self.apply(vec![OwnedSubtree::Drop(sub(name))], changes);
        self.held.empty(name);
        self.held.live.remove(&name);
    }

    /// The batch that clears `name`.
    fn clearing(&mut self, name: u8) -> OwnedBatch {
        let mut changes = Changes::default();
        changes.cleared.push(name);
        self.batch(vec![OwnedSubtree::Clear(sub(name))], changes)
    }

    /// Clears `name`.
    fn clear(&mut self, name: u8) {
        let mut changes = Changes::default();
        changes.cleared.push(name);
        self.apply(vec![OwnedSubtree::Clear(sub(name))], changes);
        self.held.empty(name);
    }

    /// Moves the first of `pairs` to another key of the Merk its host is
    /// in, and with it the subtrees below that the other pairs name.
    fn moved(&mut self, pairs: &[(u8, u8)]) {
        let (name, new) = pairs[0];
        let to: BTreeMap<u8, u8> = pairs.iter().copied().collect();
        let (parent, old_key) = self.held.live[&name].0.clone().expect("a host");
        let new_key = vec![b'm', new];
        let read = self.tx.txn.node(sub(parent), &old_key, handle_of);
        let mut changes = Changes::default();
        changes.hosted.insert((parent, new_key.clone()), new);
        let (from_key, to_key) = (old_key, new_key.clone());
        changes.edit(parent, move |items| {
            let value = items.remove(&from_key).expect("the node of the host");
            items.insert(to_key, value);
        });
        let below = pairs[1..].iter();
        let moved = OwnedSubtree::Move {
            from: sub(name),
            to: sub(new),
            host: (sub(parent), new_key.clone(), 0),
            read: read.ok().flatten(),
            below: below.map(|(old, new)| (sub(*old), sub(*new))).collect(),
        };
        self.apply(vec![moved], changes);
        let all: Vec<u8> = (std::iter::once(name).chain(self.held.below(name))).collect();
        let taken: Vec<_> = (all.into_iter())
            .map(|old| (old, self.held.live.remove(&old).expect("a live subtree")))
            .collect();
        for (old, (host, items)) in taken {
            let host = match old == name {
                true => Some((parent, new_key.clone())),
                false => host.map(|(above, key)| (to[&above], key)),
            };
            self.held.live.insert(to[&old], (host, items));
        }
    }

    /// Commits the transaction, which the test keeps: it has ended, and
    /// writes made through it afterwards are of a new one.
    fn commit_kept(&mut self) {
        let world = self.world;
        let outcome = world.db.commit(world.versions, &self.tx.txn);
        assert!(matches!(outcome, Ok(Outcome::Committed(_))), "{outcome:?}");
        self.committed = self.held.clone();
    }

    /// Commits the transaction, and begins the next.
    fn commit(&mut self) {
        self.commit_kept();
        self.tx = self.world.begin();
    }

    /// Reads a record through the transaction: an operation, which makes
    /// the frames where the transaction has writes and no frames.
    fn read(&self) {
        let mut value = Vec::new();
        let read = self.tx.txn.get(aux(TOP), b"x", &mut value);
        assert!(matches!(read, Ok(false)), "{read:?}");
    }

    /// What the index and the directory of the transaction's frames
    /// have, if it has frames. The ids that the directory has the
    /// subtrees under are kept by their names.
    fn shown(&mut self) -> Option<crate::txn::below::Shown> {
        let subtrees = NAMES.map(sub);
        let (shown, ids) = self.tx.txn.below_shown(&subtrees)?;
        for (name, id) in NAMES.into_iter().zip(ids) {
            if let Some(id) = id {
                self.names.insert(id, name);
            }
        }
        Some(shown)
    }

    /// Keeps the id of every subtree that is entered.
    fn learn(&mut self) {
        self.shown();
    }

    /// The index of the transaction's frames by the names of the
    /// subtrees, or `None` for a transaction with no frames.
    fn index(&mut self) -> Option<Index> {
        let shown = self.shown()?;
        let name = |subtree: &Subtree| subtree.as_bytes()[0];
        let listed = shown.listed.iter().map(|(subtree, above)| {
            let above = above.iter().map(|id| self.names[id]).collect();
            (name(subtree), above)
        });
        Some(Index {
            listed: listed.collect(),
            again: shown.again.iter().map(name).collect(),
            taken: shown.taken,
        })
    }

    /// The entries of the directory that were opened for the index.
    fn opened(&self) -> u64 {
        self.tx.txn.stats().below_opened
    }
}

/// `TOP` with `A` and `E` in it, `B` and `C` in `A`, and `D` in `B`, each
/// of the three innermost with a node of its own, committed.
fn planted<'w, 's>(world: &'w World<'s>) -> Grove<'w, 's> {
    let mut grove = Grove::new(world);
    grove.enter(TOP, None);
    for (name, host) in [(A, TOP), (B, A), (D, B), (C, A), (E, TOP)] {
        grove.enter(name, Some(host));
    }
    for name in [D, C, E] {
        grove.write(name, 1);
    }
    for name in [L1, L2, L3, L4, L5] {
        grove.enter(name, None);
        grove.write(name, 1);
    }
    grove.commit();
    grove
}

/// A transaction that takes no Merk with a node keeps an empty index,
/// and opens nothing for it: enters, writes, a move, a clear and a drop
/// of a Merk with no node say nothing to it.
#[test]
fn a_transaction_that_takes_no_merk_with_a_node_keeps_an_empty_index() {
    let (db, versions) = Db::new(256);
    let world = World::new(&db, &versions);
    let mut grove = planted(&world);
    assert_eq!(grove.index(), None, "no write, and no frames");
    grove.write(D, 2);
    grove.enter(X, Some(C));
    grove.moved(&[(B, B2), (D, D2)]);
    grove.write(D2, 3);
    // A Merk with no node hosts none: nothing is below it, and nothing
    // is asked.
    grove.clear(X);
    grove.drop(X);
    assert_eq!(grove.index(), index(&[], &[], 0));
    assert_eq!(grove.opened(), 0);
    // The two batches that took a Merk were held all the same.
    assert_eq!(compared(), held(2, 0, 0, 0));
    grove.commit();
}

/// The first batch that takes a Merk with a node lists every subtree the
/// set has an entry under. A batch like it after that asks a subtree
/// that the set has come to have an entry under since, one that a batch
/// made or took the entry of, and one that a Merk above it left. A
/// subtree that was dropped is then listed under nothing, and while it
/// is, no batch opens it as one below a Merk that the batch takes. One
/// that is entered again is asked again, and found below what it is under.
#[test]
fn the_index_follows_writes_enters_clears_and_drops() {
    let (db, versions) = Db::new(256);
    let world = World::new(&db, &versions);
    let mut grove = planted(&world);
    // The set has an entry under D and under what hosts it, and under E.
    grove.write(D, 2);
    grove.write(E, 2);
    assert_eq!(grove.index(), index(&[], &[], 0));

    // A drop of a Merk with a node: the five subtrees are opened and
    // listed, and none of them is below C.
    grove.drop(C);
    let all: [(u8, &[u8]); 5] = [
        (TOP, &[]),
        (A, &[TOP]),
        (B, &[TOP, A]),
        (D, &[TOP, A, B]),
        (E, &[TOP]),
    ];
    assert_eq!(grove.index(), index(&all, &[], 5));
    assert_eq!((grove.opened(), compared()), (5, held(1, 1, 0, 0)));

    // A drop above a written subtree. C, which the set has an entry
    // under since its drop, is opened and listed under nothing. D is
    // listed under B: it is opened, and is found below. Both B and D are
    // asked again.
    grove.drop(B);
    let listed: [(u8, &[u8]); 6] = [
        (TOP, &[]),
        (A, &[TOP]),
        (B, &[TOP, A]),
        (C, &[]),
        (D, &[TOP, A, B]),
        (E, &[TOP]),
    ];
    assert_eq!(grove.index(), index(&listed, &[B, D], 6));
    assert_eq!((grove.opened(), compared()), (7, held(2, 2, 1, 1)));

    // The next batch that takes a Merk with a node asks the two: neither
    // has an entry in use, and each is listed under nothing. None is to
    // be asked again.
    grove.clear(L1);
    let listed: [(u8, &[u8]); 6] = [
        (TOP, &[]),
        (A, &[TOP]),
        (B, &[]),
        (C, &[]),
        (D, &[]),
        (E, &[TOP]),
    ];
    assert_eq!(grove.index(), index(&listed, &[], 6));
    assert_eq!((grove.opened(), compared()), (9, held(3, 3, 1, 1)));

    // D is entered again, elsewhere: the enter is no batch that asks,
    // and it has D, which was to be asked no more, asked again. The
    // write into D says nothing more.
    grove.enter(D, Some(A));
    assert_eq!(grove.index(), index(&listed, &[D], 6));
    grove.write(D, 3);
    assert_eq!(grove.index(), index(&listed, &[D], 6));
    assert_eq!(grove.opened(), 9);

    // The next batch that asks opens L1, which the set has an entry
    // under since its clear, and D, which is listed where it is entered
    // now. Nothing is listed under E.
    grove.clear(E);
    let listed: [(u8, &[u8]); 7] = [
        (TOP, &[]),
        (A, &[TOP]),
        (B, &[]),
        (C, &[]),
        (D, &[TOP, A]),
        (E, &[TOP]),
        (L1, &[]),
    ];
    assert_eq!(grove.index(), index(&listed, &[], 7));
    assert_eq!((grove.opened(), compared()), (11, held(4, 4, 1, 1)));

    // A clear of a Merk with a node, above D: D alone is opened, and is
    // found below. The cleared subtree keeps its entry and stays listed
    // as it is; what was listed under the id it had is asked again.
    grove.clear(A);
    assert_eq!(grove.index(), index(&listed, &[D], 7));
    assert_eq!((grove.opened(), compared()), (12, held(5, 5, 2, 2)));

    // A drop of the outermost: D is asked first, has no entry in use any
    // more and is listed under nothing, so A and E are the two that are
    // opened for the drop.
    grove.drop(TOP);
    let listed: [(u8, &[u8]); 7] = [
        (TOP, &[]),
        (A, &[TOP]),
        (B, &[]),
        (C, &[]),
        (D, &[]),
        (E, &[TOP]),
        (L1, &[]),
    ];
    assert_eq!(grove.index(), index(&listed, &[TOP, A, E], 7));
    assert_eq!((grove.opened(), compared()), (15, held(6, 6, 3, 4)));

    // What was dropped, and what went with it, is asked once more and is
    // listed under nothing.
    grove.clear(L2);
    let gone: [(u8, &[u8]); 7] = [
        (TOP, &[]),
        (A, &[]),
        (B, &[]),
        (C, &[]),
        (D, &[]),
        (E, &[]),
        (L1, &[]),
    ];
    assert_eq!(grove.index(), index(&gone, &[], 7));
    assert_eq!(grove.opened(), 18);
    // And no more: each batch from here on opens the one subtree that
    // the batch before it came to have an entry under.
    grove.clear(L3);
    assert_eq!(grove.opened(), 19);
    grove.clear(L4);
    assert_eq!(grove.opened(), 20);
    let mut listed = gone.to_vec();
    listed.extend([(L2, &[][..]), (L3, &[][..])]);
    assert_eq!(grove.index(), index(&listed, &[], 9));
    assert_eq!(compared(), held(9, 9, 3, 4));
    grove.commit();
}

/// A move: the prefixes that the Merks are taken from are asked again
/// and are listed under nothing, and the prefixes they are brought to,
/// which the set has an entry under since the move, are listed where the
/// Merks are entered now.
#[test]
fn the_index_follows_a_move() {
    let (db, versions) = Db::new(256);
    let world = World::new(&db, &versions);
    let mut grove = planted(&world);
    grove.write(D, 2);
    grove.clear(L1);
    let listed: [(u8, &[u8]); 4] = [(TOP, &[]), (A, &[TOP]), (B, &[TOP, A]), (D, &[TOP, A, B])];
    assert_eq!(grove.index(), index(&listed, &[], 4));
    assert_eq!(grove.opened(), 4);

    // The move is no batch that asks: B and D are to be asked again,
    // and are listed as they were until they are. The Merk that was
    // under B keeps its id, which the directory has under B2 now.
    grove.moved(&[(B, B2), (D, D2)]);
    let unasked: [(u8, &[u8]); 4] = [(TOP, &[]), (A, &[TOP]), (B, &[TOP, A]), (D, &[TOP, A, B2])];
    assert_eq!(grove.index(), index(&unasked, &[B, D], 4));
    assert_eq!(grove.opened(), 4);

    // L1, B2 and D2 came since, and B and D are asked again.
    grove.clear(L2);
    let listed: [(u8, &[u8]); 7] = [
        (TOP, &[]),
        (A, &[TOP]),
        (B, &[]),
        (D, &[]),
        (B2, &[TOP, A]),
        (D2, &[TOP, A, B2]),
        (L1, &[]),
    ];
    assert_eq!(grove.index(), index(&listed, &[], 7));
    assert_eq!(grove.opened(), 9);

    // What is listed under A is what the move brought there: the two
    // are opened, and are found below.
    grove.clear(A);
    assert_eq!(grove.opened(), 12);
    assert_eq!(compared(), held(3, 3, 1, 2));
    let mut listed = listed.to_vec();
    listed.push((L2, &[]));
    assert_eq!(grove.index(), index(&listed, &[B2, D2], 8));
    grove.commit();
}

/// Frames that are made again start with an empty index, and the next
/// batch that takes a Merk with a node asks every written subtree: after
/// a rollback to a mark, after a scope that is laid out is taken out,
/// after a commit beside the transaction, and after a batch that failed
/// half way. A transaction that is given up, or that committed, has no
/// frames and no index.
#[test]
fn the_index_is_made_again_with_the_frames() {
    let (db, versions) = Db::new(256);
    let world = World::new(&db, &versions);
    let mut grove = planted(&world);
    grove.write(D, 2);
    grove.clear(L1);
    let listed: [(u8, &[u8]); 4] = [(TOP, &[]), (A, &[TOP]), (B, &[TOP, A]), (D, &[TOP, A, B])];
    assert_eq!(grove.index(), index(&listed, &[], 4));
    assert_eq!(grove.opened(), 4);

    // A rollback to a mark: the frames go, and the index with them.
    let (mark, before) = (grove.tx.txn.mark(), grove.held.clone());
    grove.write(D, 3);
    assert!(grove.tx.txn.rewind(mark));
    grove.held = before;
    assert_eq!(grove.index(), None);
    let rebuilds = grove.tx.txn.stats().rebuilds;
    grove.read();
    assert_eq!(grove.tx.txn.stats().rebuilds, rebuilds + 1);
    assert_eq!(grove.index(), index(&[], &[], 0));
    // The four, and L1, which the set has an entry under since its
    // clear.
    grove.clear(L2);
    let mut listed = listed.to_vec();
    listed.push((L1, &[]));
    assert_eq!(grove.index(), index(&listed, &[], 5));
    assert_eq!(grove.opened(), 9);

    // A scope that is laid out is taken out: the same.
    let before = grove.held.clone();
    let batch = grove.batch(Vec::new(), Grove::node(D, 4));
    let scope = grove.tx.txn.scope().expect("a scope");
    assert!(scope.is_laid_out());
    let applied = batch.through(&scope).with(|batch| scope.apply(batch));
    applied.expect("the batch is applied");
    scope.rollback();
    grove.held = before;
    assert_eq!(grove.index(), None);
    grove.read();
    assert_eq!(grove.index(), index(&[], &[], 0));
    grove.clear(L3);
    listed.push((L2, &[]));
    assert_eq!(grove.index(), index(&listed, &[], 6));
    assert_eq!(grove.opened(), 15);

    // A commit beside the transaction: the frames are laid out again
    // over its version.
    let other = world.begin();
    let beside = OwnedBatch {
        records: vec![put(aux(1), b"beside", b"it")],
        ..OwnedBatch::default()
    };
    let scope = other.txn.scope().expect("a scope");
    let applied = beside.through(&scope).with(|batch| scope.apply(batch));
    applied.expect("the record is written");
    scope.merge();
    let outcome = db.commit(&versions, &other.txn);
    assert!(matches!(outcome, Ok(Outcome::Committed(_))), "{outcome:?}");
    let rebuilds = grove.tx.txn.stats().rebuilds;
    grove.read();
    assert_eq!(grove.tx.txn.stats().rebuilds, rebuilds + 1);
    assert_eq!(grove.index(), index(&[], &[], 0));
    grove.clear(L4);
    listed.push((L3, &[]));
    assert_eq!(grove.index(), index(&listed, &[], 7));
    assert_eq!(grove.opened(), 22);

    // A batch that fails half way: it writes a node of E, and enters a
    // subtree under the node of TOP that hosts A already, which the
    // directory refuses after the tree pages took the node. The frames
    // are made again from the write set, which has nothing of the batch.
    let before = grove.held.clone();
    let taken = (sub(TOP), vec![b'h', A], 0);
    let failing = grove.batch(
        vec![OwnedSubtree::Enter(sub(X), Some(taken))],
        Grove::node(E, 9),
    );
    let (rebuilds, writes) = {
        let stats = grove.tx.txn.stats();
        (stats.rebuilds, stats.writes)
    };
    let scope = grove.tx.txn.scope().expect("a scope");
    let refused = failing.through(&scope).with(|batch| scope.apply(batch));
    assert!(
        matches!(refused, Err(TxnError::Directory(_))),
        "{refused:?}"
    );
    scope.rollback();
    grove.held = before;
    let stats = grove.tx.txn.stats();
    assert_eq!((stats.rebuilds, stats.writes), (rebuilds + 1, writes));
    assert_eq!(grove.index(), index(&[], &[], 0));
    // The four, and the four that were cleared.
    grove.clear(L5);
    listed.push((L4, &[]));
    assert_eq!(grove.index(), index(&listed, &[], 8));
    assert_eq!(grove.opened(), 30);
    assert_eq!(compared(), held(5, 5, 0, 0));

    // The transaction is given up: no frames. What it opened is still
    // counted, and what it writes afterwards starts with an empty index.
    grove.tx.txn.rollback().expect("no scope is open");
    grove.held = grove.committed.clone();
    assert_eq!(grove.index(), None);
    assert_eq!(grove.opened(), 30);
    grove.write(D, 5);
    assert_eq!(grove.index(), index(&[], &[], 0));
    grove.clear(L1);
    let listed: [(u8, &[u8]); 4] = [(TOP, &[]), (A, &[TOP]), (B, &[TOP, A]), (D, &[TOP, A, B])];
    // The count of the set's first writes does not go back where the
    // transaction ends its writes: the four came after the nine, of
    // which L5 was the last.
    assert_eq!(grove.index(), index(&listed, &[], 13));
    assert_eq!((grove.opened(), compared()), (34, held(6, 6, 0, 0)));
    // And once it has committed, the same transaction has none.
    grove.commit_kept();
    assert_eq!(grove.index(), None);
    assert_eq!(grove.opened(), 34);
}

/// A batch that is refused with the frames as they were, after the
/// frames were asked what the transaction wrote below the Merk it clears:
/// the frames stay, and the index stays as that call brought it up to
/// date. The same batch is refused again, and what is opened for it then
/// is the one subtree that is listed under the Merk.
#[test]
fn a_batch_that_is_refused_after_the_frames_were_asked_leaves_the_index_up_to_date() {
    let (db, versions) = Db::new(256);
    let world = World::new(&db, &versions);
    let mut grove = planted(&world);
    grove.write(D, 2);
    assert_eq!(grove.index(), index(&[], &[], 0));
    assert_eq!((grove.opened(), compared()), (0, held(0, 0, 0, 0)));

    // The batch clears B, which has the node that hosts D, and puts a
    // record under a subtree that is not entered.
    let before = grove.held.clone();
    let mut refused = grove.clearing(B);
    refused.records.push(put(aux(X), b"no", b"subtree"));
    grove.held = before;
    let (rebuilds, writes) = {
        let stats = grove.tx.txn.stats();
        (stats.rebuilds, stats.writes)
    };
    let listed: [(u8, &[u8]); 4] = [(TOP, &[]), (A, &[TOP]), (B, &[TOP, A]), (D, &[TOP, A, B])];
    // The first call opens the four that the set has an entry under, and
    // then D, which is listed under B. The second opens D.
    for (call, opened) in [(1, 5), (2, 6)] {
        let scope = grove.tx.txn.scope().expect("a scope");
        let applied = refused.through(&scope).with(|batch| scope.apply(batch));
        assert!(
            matches!(
                applied,
                Err(TxnError::Batch(BatchError::NotEntered { subtree })) if subtree == sub(X)
            ),
            "{applied:?}"
        );
        scope.rollback();
        let stats = grove.tx.txn.stats();
        assert_eq!((stats.rebuilds, stats.writes), (rebuilds, writes));
        assert_eq!(grove.index(), index(&listed, &[], 4));
        assert_eq!(
            (grove.opened(), compared()),
            (opened, held(call, call, call, call))
        );
    }

    // The clear by itself is applied, and finds D as the two calls did.
    grove.clear(B);
    assert_eq!(grove.index(), index(&listed, &[D], 4));
    assert_eq!((grove.opened(), compared()), (7, held(3, 3, 3, 3)));
}

/// A scope that is not laid out is taken out while the set holds other
/// writes: the frames stay, and the index with them. A subtree that the
/// set had an entry under through that scope alone stays listed until it
/// is met, and is forgotten there, unopened: it is none of the subtrees
/// that the transaction wrote.
#[test]
fn a_subtree_of_a_scope_that_was_not_laid_out_is_forgotten_where_it_is_met() {
    let (db, versions) = Db::new(256);
    let world = World::new(&db, &versions);
    let mut grove = planted(&world);
    grove.write(A, 2);
    let clearing = grove.clearing(L1);
    let first = grove.tx.txn.scope().expect("a scope");
    let second = grove.tx.txn.scope().expect("a scope");
    assert!(first.is_laid_out() && !second.is_laid_out());
    // The second scope writes a record under D, by key.
    let record = OwnedBatch {
        records: vec![put(aux(D), b"k", b"v")],
        ..OwnedBatch::default()
    };
    let applied = record.through(&second).with(|batch| second.apply(batch));
    applied.expect("the record is written");
    // The first takes a Merk with a node: D is one of the subtrees the
    // set has an entry under, and is listed.
    let applied = clearing.through(&first).with(|batch| first.apply(batch));
    applied.expect("the subtree is cleared");
    grove.held.empty(L1);
    second.rollback();
    first.merge();
    let listed: [(u8, &[u8]); 3] = [(TOP, &[]), (A, &[TOP]), (D, &[TOP, A, B])];
    assert_eq!(grove.index(), index(&listed, &[], 3));
    assert_eq!(grove.opened(), 3);
    assert_eq!(grove.tx.txn.stats().rebuilds, 0);

    // A clear above D: D is listed under A and is met, but the set has
    // no entry under it any more. It is forgotten, and is not opened:
    // L1, which came since, is the one subtree that is. The writes of
    // the scope stood before the clear of L1, which was added again
    // behind what stayed: L1 came again there, as the fifth.
    grove.clear(A);
    let listed: [(u8, &[u8]); 3] = [(TOP, &[]), (A, &[TOP]), (L1, &[])];
    assert_eq!(grove.index(), index(&listed, &[], 5));
    assert_eq!(grove.opened(), 4);
    assert_eq!(compared(), held(2, 2, 0, 0));
    grove.commit();
}

/// A scope that is not laid out is taken out with the only write of the
/// set: the transaction holds nothing any more, and its frames go with
/// the index, as at its end.
#[test]
fn a_scope_that_was_not_laid_out_takes_the_frames_with_the_last_write() {
    let (db, versions) = Db::new(256);
    let world = World::new(&db, &versions);
    let grove = planted(&world);
    let txn = &grove.tx.txn;
    assert!(txn.below_shown(&[]).is_none(), "no write, and no frames");
    let first = txn.scope().expect("a scope");
    let second = txn.scope().expect("a scope");
    assert!(first.is_laid_out() && !second.is_laid_out());
    let record = OwnedBatch {
        records: vec![put(aux(D), b"k", b"v")],
        ..OwnedBatch::default()
    };
    let applied = record.through(&second).with(|batch| second.apply(batch));
    applied.expect("the record is written");
    // The write made the frames, though it is kept in the write set
    // alone.
    assert_eq!(txn.stats().writes, 1);
    assert!(txn.below_shown(&[]).is_some());
    second.rollback();
    assert_eq!(txn.stats().writes, 0);
    assert!(txn.below_shown(&[]).is_none());
    first.rollback();
}
