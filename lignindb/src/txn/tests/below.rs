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
//!
//! The last tests are of a transaction that a commit met, whose writes
//! are taken by key: what it keeps of the written subtrees for the
//! directory of the committed version (`WrittenBelow`), and how many
//! subtrees a batch that drops or clears one asks whether they are below
//! (`ByKey::cut`). Each such batch is held to the walk over every written
//! subtree: by its answer, and by the entries of the directory it asked
//! for, each of which the walk asks for as well.

use std::collections::{BTreeMap, HashMap};

use lignin_tree::directory::MerkId;

use super::{
    leaves::{sub, Changes},
    model::{handle_of, OwnedBatch, OwnedSubtree},
    store::{Db, Outcome, ENTERED},
    subtrees::{aux, Held},
    *,
};
use crate::txn::{
    frames::held::{compared, Compared},
    txn::cut_held,
};

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

    /// Writes a record under `name`, by `value`, through `tx`.
    fn record(tx: &Tx<'s>, name: u8, value: &[u8]) {
        let record = OwnedBatch {
            records: vec![put(aux(name), b"k", value)],
            ..OwnedBatch::default()
        };
        let scope = tx.txn.scope().expect("a scope");
        let applied = record.through(&scope).with(|batch| scope.apply(batch));
        applied.expect("the record is written");
        scope.merge();
    }

    /// Whether the transaction reads the record that [`Self::record`]
    /// wrote under `name`.
    fn reads(&self, name: u8) -> bool {
        let mut value = Vec::new();
        let read = self.tx.txn.get(aux(name), b"k", &mut value);
        read.expect("a read")
    }

    /// Has a commit meet the transaction: the transaction writes a
    /// record under `TOP`, and a transaction beside it commits one under
    /// the same key. From here on the transaction has no frames, and
    /// its writes are taken by key.
    fn meet(&mut self) {
        Self::record(&self.tx, TOP, b"mine");
        let other = self.world.begin();
        Self::record(&other, TOP, b"theirs");
        let outcome = self.world.db.commit(self.world.versions, &other.txn);
        assert!(matches!(outcome, Ok(Outcome::Committed(_))), "{outcome:?}");
        self.read();
        assert!(self.tx.txn.stats().overlapped > 0);
        assert_eq!(self.index(), None, "no frames");
    }

    /// What the transaction keeps, for the directory of the committed
    /// version it read last, of the written subtrees that the version
    /// has below a Merk (`WrittenBelow`), by the names of the subtrees:
    /// the list, and the subtrees that it took in as standing as the
    /// committed ones and has not forgotten. One of them that has come
    /// to stand otherwise since is in it until a batch meets it there.
    fn kept(&mut self) -> (Option<Index>, Vec<u8>) {
        kept_by(&self.tx, &mut self.names)
    }
}

/// [`Grove::kept`] of the transaction `tx`, with the ids that were seen
/// in `names`.
fn kept_by(tx: &Tx<'_>, names: &mut HashMap<MerkId, u8>) -> (Option<Index>, Vec<u8>) {
    let subtrees = NAMES.map(sub);
    let shown = tx.txn.cut_shown(&subtrees);
    let (shown, standing, ids) = shown.expect("the directory of a version");
    for (name, id) in NAMES.into_iter().zip(ids) {
        if let Some(id) = id {
            names.insert(id, name);
        }
    }
    let name = |subtree: &Subtree| subtree.as_bytes()[0];
    let listed = shown.listed.iter().map(|(subtree, above)| {
        let above = above.iter().map(|id| names[id]).collect();
        (name(subtree), above)
    });
    let index = Index {
        listed: listed.collect(),
        again: shown.again.iter().map(name).collect(),
        taken: shown.taken,
    };
    (Some(index), standing.iter().map(name).collect())
}

fn cut(calls: u64, asked: u64, written: u64, found: u64) -> cut_held::Compared {
    cut_held::Compared {
        calls,
        asked,
        written,
        found,
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

/// A transaction that is served by key lists the written subtrees that
/// stood as the committed ones when it asked them, by the Merks the
/// committed version has them below, and a batch that drops or clears a
/// subtree asks those that are listed under its Merk, and those that the
/// transaction entered and wrote into. The transaction writes into `D`
/// and `E`, enters `X` under `C` and writes into it, and is met. Each
/// batch after that is counted: how many subtrees it asked whether they
/// are below, beside how many the set had an entry under outside the
/// subtree that the batch took. What is kept is read after some of the
/// batches, as the batch left it when it asked, before it was added:
/// after the drop of `B` it still has `B` and `D`, which that drop took.
#[test]
fn a_batch_by_key_asks_the_written_subtrees_that_can_be_below_what_it_takes() {
    let (db, versions) = Db::new(256);
    let world = World::new(&db, &versions);
    let mut grove = planted(&world);
    grove.write(D, 2);
    grove.write(E, 2);
    grove.enter(X, Some(C));
    grove.write(X, 1);
    for name in [D, X] {
        Grove::record(&grove.tx, name, b"mine");
    }
    grove.meet();
    assert_eq!(cut_held::compared(), cut(0, 0, 0, 0));

    // A clear of a subtree that nothing is below. The seven written
    // subtrees are asked how they stand: six as the committed ones, and
    // X as the transaction's own, which is not listed. X is asked
    // whether it is below, as a subtree that the transaction entered.
    grove.clear(L1);
    let all: [(u8, &[u8]); 6] = [
        (TOP, &[]),
        (A, &[TOP]),
        (B, &[TOP, A]),
        (C, &[TOP, A]),
        (D, &[TOP, A, B]),
        (E, &[TOP]),
    ];
    assert_eq!(
        grove.kept(),
        (index(&all, &[], 7), vec![TOP, A, B, C, D, E])
    );
    assert_eq!(cut_held::compared(), cut(1, 1, 7, 0));

    // A drop of B: L1, which the set has an entry under since its clear,
    // stands as the committed one, and is listed under nothing. D is
    // listed under the Merk of B, and is found below; X is asked again.
    assert!(grove.reads(D));
    grove.drop(B);
    let mut listed = all.to_vec();
    listed.push((L1, &[]));
    let standing = vec![TOP, A, B, C, D, E, L1];
    assert_eq!(grove.kept(), (index(&listed, &[], 8), standing));
    assert_eq!(cut_held::compared(), cut(1, 2, 7, 1));
    assert!(!grove.reads(D), "the record went with the subtree");

    // A drop of C: X hangs by the host that its enter states, and is
    // found below. Nothing is listed under the Merk of C.
    assert!(grove.reads(X));
    grove.drop(C);
    assert_eq!(cut_held::compared(), cut(1, 1, 7, 1));
    assert!(!grove.reads(X), "the record went with the subtree");

    // A clear of A, whose Merk has a node in the committed version: B,
    // C and D are listed under it, and none of them stands as the
    // committed one any more. They are asked, and forgotten.
    grove.clear(A);
    let listed: [(u8, &[u8]); 4] = [(TOP, &[]), (A, &[TOP]), (E, &[TOP]), (L1, &[])];
    assert_eq!(grove.kept(), (index(&listed, &[], 8), vec![TOP, A, E, L1]));
    assert_eq!(cut_held::compared(), cut(1, 4, 7, 0));

    // A commit beside the transaction: the list is of the version, and
    // the batch that asks at the next begins it again. Every written
    // subtree is asked how it stands, and the four that stand as the
    // committed ones are listed.
    let other = world.begin();
    Grove::record(&other, L5, b"beside");
    let outcome = db.commit(&versions, &other.txn);
    assert!(matches!(outcome, Ok(Outcome::Committed(_))), "{outcome:?}");
    grove.clear(L2);
    assert_eq!(grove.kept(), (index(&listed, &[], 8), vec![TOP, A, E, L1]));
    assert_eq!(cut_held::compared(), cut(1, 1, 8, 0));

    // A drop of the outermost takes what stands as the committed one
    // below it, and X, which the transaction entered there, is asked.
    grove.drop(TOP);
    assert_eq!(cut_held::compared(), cut(1, 3, 8, 2));
}

/// The list is begun again where a write is taken out of the set: a
/// subtree that did not stand as the committed one when it was asked,
/// and was not listed, can stand so again. The transaction writes into
/// `C`. A scope drops `C`, which puts it off, and clears another
/// subtree, where `C` is asked and is gone; the scope is rolled back.
/// The next batch asks every written subtree again, and a clear of `A`
/// then finds `C` below it.
#[test]
fn the_list_by_key_is_begun_again_where_a_write_is_taken_out_of_the_set() {
    let (db, versions) = Db::new(256);
    let world = World::new(&db, &versions);
    let mut grove = planted(&world);
    grove.write(C, 2);
    Grove::record(&grove.tx, C, b"mine");
    grove.meet();

    let before = grove.held.clone();
    let dropping = {
        let mut changes = Changes::default();
        let (host, key) = grove.held.live[&C].0.clone().expect("a host");
        changes.edit(host, move |items| drop(items.remove(&key)));
        grove.batch(vec![OwnedSubtree::Drop(sub(C))], changes)
    };
    let clearing = grove.clearing(L1);
    let scope = grove.tx.txn.scope().expect("a scope");
    let applied = dropping.through(&scope).with(|batch| scope.apply(batch));
    applied.expect("the subtree is dropped");
    // C is put off: the batch that asks takes it.
    let listed: [(u8, &[u8]); 3] = [(TOP, &[]), (A, &[TOP]), (C, &[])];
    let kept = kept_by(&grove.tx, &mut grove.names);
    assert_eq!(kept, (index(&listed, &[C], 3), vec![TOP, A]));
    let applied = clearing.through(&scope).with(|batch| scope.apply(batch));
    applied.expect("the subtree is cleared");
    // And asked at the next: it is gone, and is not listed.
    let listed: [(u8, &[u8]); 2] = [(TOP, &[]), (A, &[TOP])];
    let kept = kept_by(&grove.tx, &mut grove.names);
    assert_eq!(kept, (index(&listed, &[], 3), vec![TOP, A]));
    assert_eq!(cut_held::compared(), cut(2, 0, 5, 0));
    scope.rollback();
    grove.held = before;
    assert!(grove.reads(C));

    // The list is as it was until a batch asks.
    assert_eq!(grove.kept(), (index(&listed, &[], 3), vec![TOP, A]));
    grove.clear(L2);
    let listed: [(u8, &[u8]); 3] = [(TOP, &[]), (A, &[TOP]), (C, &[TOP, A])];
    // The count of the set's first writes does not go back with the
    // rollback: the subtree that the scope cleared was the fourth.
    assert_eq!(grove.kept(), (index(&listed, &[], 4), vec![TOP, A, C]));
    assert_eq!(cut_held::compared(), cut(1, 0, 3, 0));
    grove.clear(A);
    assert_eq!(cut_held::compared(), cut(1, 1, 3, 1));
    assert!(!grove.reads(C), "the record went with the subtree");
}

/// The list is of one version, and a batch that asks at another begins
/// it again: the next version can have a written subtree below another
/// Merk. The transaction writes into `E`, and a clear of another
/// subtree lists `E` under the Merk of the outermost one. A commit
/// beside the transaction drops the outermost subtree and enters it and
/// `E` again, so that the next version has `E` below a Merk with another
/// id. A drop of the outermost subtree then finds `E` below it.
#[test]
fn the_list_by_key_is_begun_again_at_another_version() {
    let (db, versions) = Db::new(256);
    let world = World::new(&db, &versions);
    let mut grove = planted(&world);
    grove.write(E, 2);
    Grove::record(&grove.tx, E, b"mine");
    grove.meet();
    let tops = |grove: &Grove<'_, '_>| grove.names.values().filter(|name| **name == TOP).count();

    grove.clear(L1);
    let listed: [(u8, &[u8]); 2] = [(TOP, &[]), (E, &[TOP])];
    assert_eq!(grove.kept(), (index(&listed, &[], 2), vec![TOP, E]));
    assert_eq!(cut_held::compared(), cut(1, 0, 2, 0));
    assert_eq!(tops(&grove), 1);

    let mut beside = Grove::new(&world);
    beside.held = grove.committed.clone();
    beside.drop(TOP);
    beside.enter(TOP, None);
    beside.enter(E, Some(TOP));
    beside.commit_kept();

    // The transaction reads at the next version. What it keeps is of
    // the version before until a batch asks, and is shown as an empty
    // list of this one.
    assert!(grove.reads(E));
    assert_eq!(grove.kept(), (index(&[], &[], 0), vec![]));
    assert_eq!(tops(&grove), 2, "the Merk of the subtree has another id");

    // The subtree of the clear has an entry in the set by now, so two
    // written subtrees are outside the one that the batch drops.
    grove.drop(TOP);
    assert_eq!(cut_held::compared(), cut(1, 1, 2, 1));
    assert!(!grove.reads(E), "the record went with the subtree");
}

/// Where nothing that the transaction has written stands as the
/// committed one, and it has entered no subtree, a batch by key that
/// drops a subtree asks the directory for nothing. The walk over every
/// written subtree asks for nothing there either, and each call is held
/// to the entries that the walk asks for. The transaction writes into
/// `E`, and drops `L1`, then the outermost subtree, with which `E` goes.
/// Nothing it has written stands as the committed one after that, and a
/// drop of `L2` finds the two that did, forgets them, and opens nothing.
#[test]
fn a_batch_by_key_opens_nothing_where_nothing_is_entered_and_nothing_written_stands_as_committed() {
    let (db, versions) = Db::new(256);
    let world = World::new(&db, &versions);
    let mut grove = planted(&world);
    grove.write(E, 2);
    grove.meet();
    grove.drop(L1);
    let listed: [(u8, &[u8]); 2] = [(TOP, &[]), (E, &[TOP])];
    assert_eq!(grove.kept(), (index(&listed, &[], 2), vec![TOP, E]));
    grove.drop(TOP);
    assert_eq!(cut_held::compared(), cut(2, 1, 4, 1));
    // TOP and E are asked how they stand, and are forgotten.
    grove.drop(L2);
    assert_eq!(grove.kept(), (index(&[], &[], 3), Vec::new()));
    assert_eq!(cut_held::compared(), cut(1, 0, 3, 0));
}

/// The transaction writes into `D` and `E`, and is met. The first batch
/// that asks clears `D`, which was not asked yet: `D` is put off. A
/// batch that writes a record asks nothing. The next clear, of `L1`,
/// which nothing is below, asks how `D` stands: as the committed one,
/// and lists it where the version has it. A batch that then clears `L2`
/// and `B` finds nothing listed under the Merk of `L2` and `D` under the
/// Merk of `B`, and `D` goes with `B`.
#[test]
fn a_deferred_subtree_is_checked_by_the_next_cut_that_does_not_take_it() {
    let (db, versions) = Db::new(256);
    let world = World::new(&db, &versions);
    let mut grove = planted(&world);
    grove.write(D, 2);
    grove.write(E, 2);
    grove.meet();

    grove.clear(D);
    let listed: [(u8, &[u8]); 5] = [
        (TOP, &[]),
        (A, &[TOP]),
        (B, &[TOP, A]),
        (D, &[]),
        (E, &[TOP]),
    ];
    assert_eq!(grove.kept(), (index(&listed, &[D], 5), vec![TOP, A, B, E]));
    assert_eq!(cut_held::compared(), cut(1, 0, 4, 0));
    Grove::record(&grove.tx, D, b"mine");

    grove.clear(L1);
    let listed: [(u8, &[u8]); 5] = [
        (TOP, &[]),
        (A, &[TOP]),
        (B, &[TOP, A]),
        (D, &[TOP, A, B]),
        (E, &[TOP]),
    ];
    let standing = vec![TOP, A, B, D, E];
    assert_eq!(grove.kept(), (index(&listed, &[], 5), standing));
    assert_eq!(cut_held::compared(), cut(1, 0, 5, 0));

    assert!(grove.reads(D));
    let mut changes = Changes::default();
    changes.cleared.extend([L2, B]);
    let both = vec![OwnedSubtree::Clear(sub(L2)), OwnedSubtree::Clear(sub(B))];
    grove.apply(both, changes);
    for name in [L2, B] {
        grove.held.empty(name);
    }
    assert_eq!(cut_held::compared(), cut(1, 1, 5, 1));
    assert!(!grove.reads(D), "the record went with the subtree");
}
